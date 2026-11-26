//! The checksum MapleStory patch files use: CRC-32 with polynomial 0x04C11DB7, processed
//! most-significant bit first, starting from 0, with no final inversion.

/// The polynomial, without its x^32 term.
pub const POLY: u32 = 0x04C1_1DB7;

/// `TABLES[0]` is the plain byte-at-a-time table; `TABLES[k]` advances a byte through `k`
/// further zero bytes, which lets eight input bytes be folded in one step.
static TABLES: [[u32; 256]; 8] = build_tables();

const fn build_tables() -> [[u32; 256]; 8] {
    let mut tables = [[0u32; 256]; 8];
    let mut i = 0;
    while i < 256 {
        let mut remainder = (i as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            remainder = if remainder & 0x8000_0000 != 0 { (remainder << 1) ^ POLY } else { remainder << 1 };
            bit += 1;
        }
        tables[0][i] = remainder;
        i += 1;
    }
    let mut k = 1;
    while k < 8 {
        let mut i = 0;
        while i < 256 {
            let prev = tables[k - 1][i];
            tables[k][i] = tables[0][(prev >> 24) as usize] ^ (prev << 8);
            i += 1;
        }
        k += 1;
    }
    tables
}

/// Continues a checksum over more data. Start with `0`.
pub fn update(crc: u32, data: &[u8]) -> u32 {
    #[cfg(target_arch = "x86_64")]
    if data.len() >= clmul::MIN_LEN && clmul::available() {
        // SAFETY: `available` just confirmed the CPU features `clmul::update` is built for.
        return unsafe { clmul::update(crc, data) };
    }
    update_tables(crc, data)
}

/// The table-driven version: eight bytes per step. Used where carry-less multiplication is
/// missing, and for inputs or tails too short to fold.
fn update_tables(mut crc: u32, data: &[u8]) -> u32 {
    let (chunks, rest) = data.as_chunks::<8>();
    for chunk in chunks {
        let head = crc ^ u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        crc = TABLES[7][(head >> 24) as usize]
            ^ TABLES[6][(head >> 16 & 0xff) as usize]
            ^ TABLES[5][(head >> 8 & 0xff) as usize]
            ^ TABLES[4][(head & 0xff) as usize]
            ^ TABLES[3][chunk[4] as usize]
            ^ TABLES[2][chunk[5] as usize]
            ^ TABLES[1][chunk[6] as usize]
            ^ TABLES[0][chunk[7] as usize];
    }
    for &byte in rest {
        crc = (crc << 8) ^ TABLES[0][((crc >> 24) as u8 ^ byte) as usize];
    }
    crc
}

/// The same checksum with carry-less multiplication (PCLMULQDQ), after "Fast CRC Computation
/// for Generic Polynomials Using PCLMULQDQ Instruction" (Intel, 2009).
///
/// The data is read as one long polynomial, first byte highest. A 128-bit piece `v` that
/// sits `d` bits above the data that follows can be replaced by `hi * (x^(d+64) mod P) +
/// lo * (x^d mod P)`, where `hi` and `lo` are its two halves, without changing the
/// remainder; that sum fits in 128 bits again. Four such pieces are carried side by side,
/// each folded 512 bits forward per 64 bytes of input, so the multiplications do not wait
/// on each other.
#[cfg(target_arch = "x86_64")]
mod clmul {
    use core::arch::x86_64::*;

    /// Shorter inputs go to the tables: the four lanes need 64 bytes to start.
    pub(super) const MIN_LEN: usize = 64;

    // x^n mod P for the distances data is folded by; a test recomputes them.
    pub(super) const X128: u32 = 0xE8A4_5605;
    pub(super) const X192: u32 = 0xC5B9_CD4C;
    pub(super) const X512: u32 = 0xE622_8B11;
    pub(super) const X576: u32 = 0x8833_794C;

    /// Whether this CPU can run [`update`]. The standard library caches the CPUID answer,
    /// so each call is a load and a bit test.
    pub(super) fn available() -> bool {
        is_x86_feature_detected!("pclmulqdq") && is_x86_feature_detected!("ssse3")
    }

    /// Reads 16 bytes with the first byte in the most significant position.
    #[inline]
    #[target_feature(enable = "ssse3")]
    fn load(block: &[u8; 16]) -> __m128i {
        let reverse = _mm_set_epi8(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15);
        // SAFETY: `block` is 16 readable bytes and the load has no alignment requirement.
        let bytes = unsafe { _mm_loadu_si128(block.as_ptr().cast()) };
        _mm_shuffle_epi8(bytes, reverse)
    }

    /// Moves `value` forward by the distance `by` was built for, see [`distance`].
    #[inline]
    #[target_feature(enable = "pclmulqdq")]
    fn fold(value: __m128i, by: __m128i) -> __m128i {
        let low = _mm_clmulepi64_si128(value, by, 0x00);
        let high = _mm_clmulepi64_si128(value, by, 0x11);
        _mm_xor_si128(low, high)
    }

    /// The multipliers for one fold distance: `low` is x^d mod P, `high` is x^(d+64) mod P.
    #[inline]
    #[target_feature(enable = "sse2")]
    fn distance(high: u32, low: u32) -> __m128i {
        _mm_set_epi64x(high as i64, low as i64)
    }

    /// Same result as [`super::update_tables`].
    ///
    /// # Safety
    /// The CPU must support PCLMULQDQ and SSSE3, see [`available`].
    #[target_feature(enable = "pclmulqdq,ssse3")]
    pub(super) unsafe fn update(crc: u32, data: &[u8]) -> u32 {
        let (blocks, tail) = data.as_chunks::<16>();
        let (groups, singles) = blocks.as_chunks::<4>();
        let Some((first, groups)) = groups.split_first() else {
            return super::update_tables(crc, data);
        };

        // The checksum so far goes on top of the first four bytes, as in the table version.
        let mut lanes = first.map(|block| load(&block));
        lanes[0] = _mm_xor_si128(lanes[0], _mm_set_epi32(crc as i32, 0, 0, 0));

        let by_512 = distance(X576, X512);
        for group in groups {
            for (lane, block) in lanes.iter_mut().zip(group) {
                *lane = _mm_xor_si128(fold(*lane, by_512), load(block));
            }
        }

        // Merge the lanes front to back, then take the 16-byte blocks that are left.
        let by_128 = distance(X192, X128);
        let mut value = lanes[0];
        for lane in &lanes[1..] {
            value = _mm_xor_si128(fold(value, by_128), *lane);
        }
        for block in singles {
            value = _mm_xor_si128(fold(value, by_128), load(block));
        }

        // `value` now has the same remainder as everything read so far. Running it through
        // the tables from zero reduces it; the tail continues from there.
        let high = _mm_cvtsi128_si64(_mm_unpackhi_epi64(value, value)) as u64;
        let low = _mm_cvtsi128_si64(value) as u64;
        let crc = super::update_tables(0, &high.to_be_bytes());
        let crc = super::update_tables(crc, &low.to_be_bytes());
        super::update_tables(crc, tail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bit-by-bit definition of the checksum, used to check the faster versions.
    fn reference(mut crc: u32, data: &[u8]) -> u32 {
        for &byte in data {
            crc ^= (byte as u32) << 24;
            for _ in 0..8 {
                crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ POLY } else { crc << 1 };
            }
        }
        crc
    }

    fn sample(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i as u32).wrapping_mul(2_654_435_761).to_le_bytes()[1]).collect()
    }

    /// SplitMix64: small, deterministic, and good enough to fill buffers.
    struct Random(u64);

    impl Random {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next() % bound as u64) as usize
        }

        /// A starting checksum that is never zero.
        fn crc(&mut self) -> u32 {
            (self.next() >> 32) as u32 | 1
        }

        fn bytes(&mut self, len: usize) -> Vec<u8> {
            let mut out = Vec::with_capacity(len + 8);
            while out.len() < len {
                out.extend(self.next().to_le_bytes());
            }
            out.truncate(len);
            out
        }
    }

    /// Checks both the dispatching `update` and the table version against the definition.
    fn check(crc: u32, data: &[u8], what: &str) {
        let expected = reference(crc, data);
        assert_eq!(update(crc, data), expected, "update, {what}, length {}", data.len());
        assert_eq!(update_tables(crc, data), expected, "tables, {what}, length {}", data.len());
    }

    #[test]
    fn matches_the_bitwise_definition_at_every_length() {
        let data = sample(100);
        for len in 0..data.len() {
            assert_eq!(update(0, &data[..len]), reference(0, &data[..len]), "length {len}");
        }
    }

    #[test]
    fn splitting_the_input_does_not_change_the_result() {
        // A regression seen in another port: feeding data in pieces counted a block twice.
        let data = sample(10_000);
        let whole = update(0, &data);
        for split in [1, 7, 8, 9, 4096, 9_999] {
            let (a, b) = data.split_at(split);
            assert_eq!(update(update(0, a), b), whole, "split at {split}");
        }
    }

    #[test]
    fn empty_input_leaves_the_checksum_unchanged() {
        assert_eq!(update(0, &[]), 0);
        assert_eq!(update(0x1234_5678, &[]), 0x1234_5678);
    }

    #[test]
    fn random_content_matches_at_every_length_up_to_1100() {
        let mut random = Random(1);
        for len in 0..=1100 {
            let data = random.bytes(len);
            check(0, &data, "from zero");
            check(random.crc(), &data, "from a random checksum");
        }
    }

    #[test]
    fn large_random_lengths_match() {
        let mut random = Random(2);
        let data = random.bytes(4 << 20);
        for _ in 0..40 {
            let len = random.below(data.len() + 1);
            let start = random.below(data.len() - len + 1);
            check(random.crc(), &data[start..start + len], "large");
        }
        check(0, &data, "whole buffer");
    }

    #[test]
    fn unaligned_slices_match() {
        let mut random = Random(3);
        let data = random.bytes(5000);
        for offset in 0..=33 {
            for len in [0, 1, 15, 16, 63, 64, 65, 127, 128, 129, 1000, 4096, 4900] {
                check(random.crc(), &data[offset..offset + len], "unaligned");
            }
        }
        check(0, &data[1..], "from offset 1");
        check(0, &data[7..], "from offset 7");
    }

    #[test]
    fn random_splits_match_a_single_call() {
        let mut random = Random(4);
        let data = random.bytes(300_000);
        for round in 0..200 {
            let len = if round % 2 == 0 { random.below(2000) } else { random.below(data.len() + 1) };
            let data = &data[..len];
            let start = if round % 4 == 0 { 0 } else { random.crc() };

            let mut crc = start;
            let mut rest = data;
            while !rest.is_empty() {
                // Mostly short pieces, so the fast and the table path alternate.
                let limit = if random.below(4) == 0 { rest.len() } else { rest.len().min(200) };
                let (piece, after) = rest.split_at(random.below(limit + 1));
                crc = update(crc, piece);
                rest = after;
            }
            assert_eq!(crc, update(start, data), "round {round}, length {len}");
            assert_eq!(crc, reference(start, data), "round {round}, length {len}");
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn folding_constants_are_powers_of_x_modulo_the_polynomial() {
        /// x^n mod P, one multiplication by x at a time.
        fn x_to_the(n: u32) -> u32 {
            let mut remainder = 1u32;
            for _ in 0..n {
                let carry = remainder & 0x8000_0000 != 0;
                remainder <<= 1;
                if carry {
                    remainder ^= POLY;
                }
            }
            remainder
        }

        assert_eq!(x_to_the(32), POLY);
        assert_eq!(clmul::X128, x_to_the(128));
        assert_eq!(clmul::X192, x_to_the(192));
        assert_eq!(clmul::X512, x_to_the(512));
        assert_eq!(clmul::X576, x_to_the(576));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn the_folding_version_matches_when_called_directly() {
        if !clmul::available() {
            eprintln!("skipped: this CPU has no PCLMULQDQ");
            return;
        }
        let mut random = Random(5);
        let data = random.bytes(70_000);
        for len in (0..600).chain([4095, 4096, 4097, 65_535, 65_536, 70_000]) {
            let crc = random.crc();
            // SAFETY: the CPU features were checked above.
            let folded = unsafe { clmul::update(crc, &data[..len]) };
            assert_eq!(folded, reference(crc, &data[..len]), "length {len}");
        }
    }

    /// Prints throughput of both versions. Run with
    /// `cargo test --release -p mfp-engine --lib crc -- --ignored --nocapture`.
    #[test]
    #[ignore = "measurement, not a check"]
    fn throughput() {
        use std::hint::black_box;
        use std::time::Instant;

        fn measure(name: &str, data: &[u8], run: impl Fn(u32, &[u8]) -> u32) -> u32 {
            let mut best = f64::MAX;
            let mut crc = 0;
            for _ in 0..5 {
                let started = Instant::now();
                crc = black_box(run(black_box(0), black_box(data)));
                best = best.min(started.elapsed().as_secs_f64());
            }
            println!("{name}: {:.2} GB/s", data.len() as f64 / 1e9 / best);
            crc
        }

        let data = Random(6).bytes(256 << 20);
        let tables = measure("tables (fallback)", &data, update_tables);
        #[cfg(target_arch = "x86_64")]
        println!("pclmulqdq + ssse3 available: {}", clmul::available());
        let dispatched = measure("update (dispatched)", &data, update);
        assert_eq!(tables, dispatched);

        // The patcher feeds the checksum in read-buffer sized pieces.
        let pieces = measure("update, 64 KiB pieces", &data, |crc, data| data.chunks(64 << 10).fold(crc, update));
        assert_eq!(tables, pieces);
    }
}
