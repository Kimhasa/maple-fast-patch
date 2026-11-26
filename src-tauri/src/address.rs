//! The address of a patch file, as the user may give one: which addresses the app fetches a
//! patch from, and which it does not.
//!
//! A patch file decides what the game's programs become, and nothing in it proves who made
//! it. Where it is fetched from is what there is to go by. So the app takes a patch from
//! Nexon's servers and from nowhere else, over a line nobody can change what is sent on, and
//! it is the app that fetches it: a file that was handed on from somewhere is not taken.
//!
//! The rule is applied here, to the address as text, before anything is asked of a server,
//! and again to an address that is read back from a record.

/// An address the app fetches a patch from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Taken {
    /// The address as it is asked for: the name of the server in small letters, and
    /// nothing in it that says nothing.
    pub(crate) url: String,
    /// It is where the client's own patch files are published. Any other address on a
    /// server of Nexon's may be anything Nexon publishes: the app cannot vouch for it being
    /// a patch for this game, and the user is asked before it is used.
    pub(crate) own: bool,
}

/// Why an address is not one the app fetches a patch from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NotTaken {
    /// It is not the address of a patch file at all.
    NotAnAddress,
    /// It is on a server that is not Nexon's.
    NotNexons,
}

/// Whether the server named `host` is one of Nexon's: one under the names Nexon publishes
/// its files and its pages under. A name that only ends in the same letters is another
/// name, and so is one written with letters that only look the same.
fn nexons(host: &str) -> bool {
    const NEXONS: [&str; 2] = ["nexoncdn.co.kr", "nexon.com"];
    let plain =
        !host.is_empty() && host.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'));
    plain && NEXONS.iter().any(|own| host == *own || host.strip_suffix(own).is_some_and(|under| under.ends_with('.')))
}

/// Whether `path` is one a patch file is published under, written the one way there is to
/// write it: folders and a file of plain names, with nothing that leads back up, nothing
/// that is written as a code for something else, and nothing after the file's name. What a
/// server makes of an address that is written another way is not what the address says.
fn plain(path: &str) -> bool {
    let named = |part: &str| {
        !matches!(part, "" | "." | "..")
            && part.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    };
    path.strip_prefix('/').is_some_and(|parts| parts.split('/').all(named)) && path.ends_with(".patch")
}

/// What `address` is taken for, or why it is not taken. `own` is where the patch files of
/// the client are published: what their addresses begin with, before the folder of a
/// version.
///
/// An address is taken when all of this holds:
///
/// - Its server is one of Nexon's (see [`nexons`]), named and nothing else: no name and
///   password before it, and no port of its own.
/// - Its path is that of a patch file, written plainly (see [`plain`]).
///
/// What is asked for is always the HTTPS address. One that is given with `http://` is
/// taken for the same address with `https://`, and is otherwise held against the rule like
/// one that was given that way: the links to Nexon's patch files that are handed round,
/// and the tools people know, write them with `http://`, and the server answers to both.
/// Nothing is fetched over plain HTTP for it.
///
/// The one server `own` names is taken as it is written, whatever it is written with. In
/// a build that is given out that is Nexon's own, an HTTPS address. A build made for trying
/// the app out may be pointed at a made-up server on this PC, which speaks plain HTTP on a
/// port of its own.
pub(crate) fn take(address: &str, own: &str) -> Result<Taken, NotTaken> {
    use NotTaken::{NotAnAddress, NotNexons};
    let (scheme, rest) = address.trim().split_once("://").ok_or(NotAnAddress)?;
    let (server, path) = rest.split_at(rest.find('/').filter(|at| *at > 0).ok_or(NotAnAddress)?);
    let (scheme, server) = (scheme.to_ascii_lowercase(), server.to_ascii_lowercase());
    let (own_server, own_folder) =
        own.split_at(own.find("://").and_then(|at| Some(at + 3 + own[at + 3..].find('/')?)).unwrap_or(own.len()));
    let own_server = own_server.to_ascii_lowercase();
    let as_given = format!("{scheme}://{server}");
    let asked_at = if as_given == own_server {
        as_given
    } else {
        if scheme != "https" && scheme != "http" {
            return Err(NotAnAddress);
        }
        // The port every HTTPS address has anyway says nothing. Any other port is a port
        // of the address's own, and so is the one plain HTTP has.
        let server = if scheme == "https" { server.strip_suffix(":443").unwrap_or(&server) } else { &server };
        // What stands before an `@` is a name to sign in with, and is no part of where the
        // address leads. It is put there to make an address look like another one.
        let named = server.rsplit('@').next().unwrap_or_default();
        let host = named.split(':').next().unwrap_or_default();
        if !nexons(host) {
            return Err(NotNexons);
        }
        if host != server {
            return Err(NotAnAddress);
        }
        format!("https://{host}")
    };
    if !plain(path) {
        return Err(NotAnAddress);
    }
    let own = asked_at == own_server && path.starts_with(&format!("{own_folder}/"));
    Ok(Taken { url: format!("{asked_at}{path}"), own })
}

/// The name of the file an address leads to: what follows its last `/`.
pub(crate) fn file_at(address: &str) -> &str {
    address.rsplit('/').next().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where the live game's patch files are published.
    const LIVE: &str = "https://maplestory.dn.nexoncdn.co.kr/Patch";

    fn taken(url: &str, own: bool) -> Result<Taken, NotTaken> {
        Ok(Taken { url: url.to_owned(), own })
    }

    #[test]
    fn an_address_among_the_clients_own_patch_files_is_taken_as_it_is() {
        let main = "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch";
        let minor = "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/Minor/03to04.patch";
        for address in [main, minor] {
            assert_eq!(take(address, LIVE), taken(address, true), "{address}");
        }
        assert_eq!(mfp_kms::LIVE.patch_files(), LIVE);
        assert_eq!(take(&mfp_kms::LIVE.patch_url(420, 421), LIVE), taken(main, true));
        assert_eq!(take(&mfp_kms::LIVE.minor_patch_url(421, 3, 4), LIVE), taken(minor, true));
        // The test world's own are under another folder of the same server.
        let test = mfp_kms::TEST.patch_url(1207, 1208);
        assert_eq!(take(&test, mfp_kms::TEST.patch_files()), taken(&test, true));
    }

    /// The same address can be written in more than one way. What is asked for is the one
    /// way, whichever was given.
    #[test]
    fn an_address_is_asked_for_in_one_spelling_however_it_was_given() {
        let asked_for = "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch";
        let given = [
            "HTTPS://MAPLESTORY.DN.NEXONCDN.CO.KR/Patch/00421/00420to00421.patch",
            "https://MapleStory.dn.NexonCDN.co.kr:443/Patch/00421/00420to00421.patch",
            "  https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch\r\n",
        ];
        for address in given {
            assert_eq!(take(address, LIVE), taken(asked_for, true), "{address:?}");
        }
    }

    /// The links to Nexon's patch files that are handed round are written with `http://`.
    /// Such an address is taken for the same one with `https://`, which is what is asked
    /// for, and from there on it is an address like any other: the client's own where it
    /// is that, one to ask about where it is Nexon's and not that.
    #[test]
    fn an_address_of_nexons_given_with_http_is_taken_as_the_same_address_with_https() {
        let own = "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch";
        let given = [
            "http://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch",
            "HTTP://MapleStory.DN.NexonCDN.co.kr/Patch/00421/00420to00421.patch",
            " http://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch ",
        ];
        for address in given {
            assert_eq!(take(address, LIVE), taken(own, true), "{address:?}");
        }
        let minor = "http://maplestory.dn.nexoncdn.co.kr/Patch/00421/Minor/03to04.patch";
        assert_eq!(
            take(minor, LIVE),
            taken("https://maplestory.dn.nexoncdn.co.kr/Patch/00421/Minor/03to04.patch", true)
        );
        let elsewhere = [
            (
                "http://maplestory.dn.nexoncdn.co.kr/PatchT/01208/01207to01208.patch",
                "https://maplestory.dn.nexoncdn.co.kr/PatchT/01208/01207to01208.patch",
            ),
            (
                "http://download.nexon.com/maplestory/00420to00421.patch",
                "https://download.nexon.com/maplestory/00420to00421.patch",
            ),
        ];
        for (address, asked_for) in elsewhere {
            assert_eq!(take(address, LIVE), taken(asked_for, false), "{address}");
        }

        // It is held against the rule like one given with https, and is no way round it.
        let not_nexons = [
            "http://example.com/Patch/00421/00420to00421.patch",
            "http://evilnexon.com/Patch/00421/00420to00421.patch",
            "http://nexon.com.example.org/Patch/00421/00420to00421.patch",
            "http://maplestory.dn.nexoncdn.co.kr@example.com/Patch/00421/00420to00421.patch",
            "http://maplestory.dn.nexоncdn.co.kr/Patch/00421/00420to00421.patch",
            "http://127.0.0.1/Patch/00421/00420to00421.patch",
            "http://127.0.0.1:18421/Patch/00421/00420to00421.patch",
        ];
        for address in not_nexons {
            assert_eq!(take(address, LIVE), Err(NotTaken::NotNexons), "{address}");
        }
        let no_address = [
            // A port of its own: the one plain HTTP has, the one HTTPS has, any other.
            "http://maplestory.dn.nexoncdn.co.kr:80/Patch/00421/00420to00421.patch",
            "http://maplestory.dn.nexoncdn.co.kr:443/Patch/00421/00420to00421.patch",
            "http://maplestory.dn.nexoncdn.co.kr:8080/Patch/00421/00420to00421.patch",
            "http://user@maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch",
            "http://maplestory.dn.nexoncdn.co.kr/Patch/../PatchT/01208/01207to01208.patch",
            "http://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch?from=example.com",
            "http://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.exe",
            "http://maplestory.dn.nexoncdn.co.kr",
            "http:///Patch/00421/00420to00421.patch",
            // Other ways of writing where something is are not that of the web.
            "ftp://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch",
            "httpx://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch",
            "ws://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch",
        ];
        for address in no_address {
            assert_eq!(take(address, LIVE), Err(NotTaken::NotAnAddress), "{address}");
        }
        // Whatever is taken is asked for over HTTPS, in a build that is given out.
        for address in given.iter().chain(elsewhere.iter().map(|(address, _)| address)) {
            assert!(take(address, LIVE).is_ok_and(|taken| taken.url.starts_with("https://")), "{address}");
        }
    }

    /// Nexon's, and not where this client's patches are: the other client's patches, a
    /// folder Nexon may publish patches under one day, any other server of Nexon's.
    #[test]
    fn an_address_on_a_server_of_nexons_outside_the_clients_patch_files_is_taken_to_be_asked_about() {
        let elsewhere = [
            "https://maplestory.dn.nexoncdn.co.kr/PatchT/01208/01207to01208.patch",
            "https://maplestory.dn.nexoncdn.co.kr/NewPatch/00421/00420to00421.patch",
            // A folder that only begins with the same letters, or is written in other ones.
            "https://maplestory.dn.nexoncdn.co.kr/Patchwork/00420to00421.patch",
            "https://maplestory.dn.nexoncdn.co.kr/patch/00421/00420to00421.patch",
            "https://maplestory.dn.nexoncdn.co.kr/00420to00421.patch",
            "https://other.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch",
            "https://nexoncdn.co.kr/Patch/00421/00420to00421.patch",
            "https://download.nexon.com/maplestory/00420to00421.patch",
            "https://nexon.com/00420to00421.patch",
        ];
        for address in elsewhere {
            assert_eq!(take(address, LIVE), taken(address, false), "{address}");
        }
    }

    #[test]
    fn an_address_on_a_server_that_is_not_nexons_is_not_taken() {
        let others = [
            "https://example.com/Patch/00421/00420to00421.patch",
            "https://127.0.0.1/Patch/00421/00420to00421.patch",
            "https://localhost/Patch/00421/00420to00421.patch",
            // Names that end in the same letters, or begin with them.
            "https://evilnexon.com/Patch/00421/00420to00421.patch",
            "https://notnexoncdn.co.kr/Patch/00421/00420to00421.patch",
            "https://nexon.com.example.org/Patch/00421/00420to00421.patch",
            "https://maplestory.dn.nexoncdn.co.kr.example.org/Patch/00421/00420to00421.patch",
            "https://nexon.com./Patch/00421/00420to00421.patch",
            "https://.nexon.com.example.org/00420to00421.patch",
            // A name to sign in with, made to look like the server.
            "https://maplestory.dn.nexoncdn.co.kr@example.com/Patch/00421/00420to00421.patch",
            "https://maplestory.dn.nexoncdn.co.kr:443@example.com/Patch/00421/00420to00421.patch",
            "https://nexon.com:x@example.com:8443/00420to00421.patch",
            // Letters that look like Nexon's and are others, written out or as the code
            // such a name is registered under.
            "https://maplestory.dn.nex\u{43e}ncdn.co.kr/Patch/00421/00420to00421.patch",
            "https://n\u{435}xon.com/00420to00421.patch",
            "https://nexon\u{ff0e}com/00420to00421.patch",
            "https://xn--nxon-5cd.com/00420to00421.patch",
            "https://maplestory.dn.xn--nexncdn-b3d.co.kr/Patch/00421/00420to00421.patch",
            "https://nexon_com/00420to00421.patch",
        ];
        for address in others {
            assert_eq!(take(address, LIVE), Err(NotTaken::NotNexons), "{address}");
        }
    }

    #[test]
    fn what_is_not_the_address_of_a_patch_file_is_not_taken() {
        let none_of_them = [
            "",
            "   ",
            "00420to00421.patch",
            r"C:\patches\00420to00421.patch",
            "maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch",
            "//maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch",
            "https://maplestory.dn.nexoncdn.co.kr",
            "https:///Patch/00421/00420to00421.patch",
            // Not an address of the web.
            "ftp://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch",
            "file:///C:/patches/00420to00421.patch",
            "javascript://maplestory.dn.nexoncdn.co.kr/Patch/00420to00421.patch",
            // A port of its own, or a name to sign in with.
            "https://maplestory.dn.nexoncdn.co.kr:8443/Patch/00421/00420to00421.patch",
            "https://maplestory.dn.nexoncdn.co.kr:80/Patch/00421/00420to00421.patch",
            "https://user@maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch",
            "https://user:word@maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch",
            r"https://example.com\@maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch",
            // No patch file.
            "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/",
            "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.exe",
            "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.PATCH",
            "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch/",
            "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch.exe",
            // A path that leads back up, or is written so that it may.
            "https://maplestory.dn.nexoncdn.co.kr/Patch/../PatchT/01208/01207to01208.patch",
            "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/../../00420to00421.patch",
            "https://maplestory.dn.nexoncdn.co.kr/Patch/./00421/00420to00421.patch",
            "https://maplestory.dn.nexoncdn.co.kr/Patch//00421/00420to00421.patch",
            "https://maplestory.dn.nexoncdn.co.kr/Patch/%2e%2e/PatchT/01208/01207to01208.patch",
            "https://maplestory.dn.nexoncdn.co.kr/Patch/00421%2f00420to00421.patch",
            r"https://maplestory.dn.nexoncdn.co.kr/Patch\..\PatchT\01207to01208.patch",
            // Something after the name of the file, or in it that no name of Nexon's has.
            "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch?from=example.com",
            "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.exe?name=.patch",
            "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch#part",
            "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420 to 00421.patch",
            "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421\n.patch",
            "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/\u{d328}\u{ce58}.patch",
            "https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421;.patch",
        ];
        for address in none_of_them {
            assert_eq!(take(address, LIVE), Err(NotTaken::NotAnAddress), "{address:?}");
        }
    }

    /// A build made for trying the app out is pointed at a made-up server on this PC, which
    /// speaks plain HTTP on a port of its own. That one server is taken as the client's own
    /// is, and nothing else is fetched over plain HTTP for it. A build that is given out
    /// has no such server: there the client's own is Nexon's, and nothing is fetched over
    /// plain HTTP at all.
    #[test]
    fn the_server_that_stands_in_for_nexons_is_taken_and_no_other_over_plain_http() {
        let stand_in = "http://127.0.0.1:18421/Patch";
        let own = "http://127.0.0.1:18421/Patch/00901/00900to00901.patch";
        assert_eq!(take(own, stand_in), taken(own, true));
        let beside = "http://127.0.0.1:18421/PatchT/01901/01900to01901.patch";
        assert_eq!(take(beside, stand_in), taken(beside, false));
        // Nexon's own are Nexon's still, and are no longer this client's own. They are
        // asked for over HTTPS, however they were given.
        let nexons = "https://maplestory.dn.nexoncdn.co.kr/Patch/00901/00900to00901.patch";
        assert_eq!(take(nexons, stand_in), taken(nexons, false));
        assert_eq!(
            take("http://maplestory.dn.nexoncdn.co.kr/Patch/00901/00900to00901.patch", stand_in),
            taken(nexons, false)
        );
        let others = [
            ("http://127.0.0.1:18422/Patch/00901/00900to00901.patch", NotTaken::NotNexons),
            ("http://127.0.0.1/Patch/00901/00900to00901.patch", NotTaken::NotNexons),
            ("https://127.0.0.1:18421/Patch/00901/00900to00901.patch", NotTaken::NotNexons),
            ("http://localhost:18421/Patch/00901/00900to00901.patch", NotTaken::NotNexons),
            ("http://127.0.0.1:18421/Patch/../secret/00900to00901.patch", NotTaken::NotAnAddress),
            ("http://127.0.0.1:18421@example.com/Patch/00901/00900to00901.patch", NotTaken::NotNexons),
        ];
        for (address, why) in others {
            assert_eq!(take(address, stand_in), Err(why), "{address}");
            // To a build that is given out, the made-up server is no server of Nexon's.
            assert!(take(address, LIVE).is_err(), "{address}");
        }
        // Plain HTTP is fetched from the made-up server only, and only by a build that is
        // pointed at it.
        assert_eq!(take(own, LIVE), Err(NotTaken::NotNexons));
        for address in [own, beside] {
            assert!(take(address, stand_in).is_ok_and(|taken| taken.url.starts_with("http://127.0.0.1:18421/")));
        }
    }

    #[test]
    fn the_file_an_address_leads_to_is_named_by_what_follows_its_last_slash() {
        assert_eq!(
            file_at("https://maplestory.dn.nexoncdn.co.kr/Patch/00421/00420to00421.patch"),
            "00420to00421.patch"
        );
        assert_eq!(file_at("https://maplestory.dn.nexoncdn.co.kr/Patch/00421/Minor/03to04.patch"), "03to04.patch");
    }
}
