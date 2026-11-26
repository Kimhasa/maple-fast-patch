//! Writes the report: what was found, line by line, the verdict, and what the diagnostic
//! itself did on the PC.
//!
//! The report is made to be passed on, so this is the one place where text from Windows or
//! from a folder turns into text that is shown, and every such piece goes through the
//! [`Scrub`] here. What must never be in a report is not in [`Facts`] to begin with: no
//! serial number, no address of a network card or of the PC, no list of what runs on it.

use crate::compare::Tally;
use crate::facts::{
    Adapter, Asked, Comparison, Fact, Facts, How, Landing, Programs, Unknown, Volume, WriteTest, Wrote,
};
use crate::known::Source;
use crate::privacy::Scrub;
use crate::units::{count, gb, korean_time, line_speed, size, speed, took};
use crate::verdict::{Evidence, MAIN_PATCH_WRITES, Verdict, verdict};

/// What the app is called.
const APP_NAME: &str = "Maple Fast Patch";
/// The most names of files which a comparison lists where it does not list them all.
const NAMES_SHOWN: usize = 20;

/// Why a fact is not in the report.
fn unknown(why: Unknown) -> String {
    match why {
        Unknown::Windows(code) => format!("확인 불가(오류 {code})"),
        Unknown::Http(status) => format!("확인 불가(HTTP {status})"),
        Unknown::NoConnection => "확인 불가(연결 실패)".to_owned(),
        Unknown::NoAnswer => "확인 불가".to_owned(),
        Unknown::Skipped => "건너뜀".to_owned(),
    }
}

/// A fact as text, or why there is none.
fn or_unknown<T>(fact: &Fact<T>, shown: impl FnOnce(&T) -> String) -> String {
    match fact {
        Ok(found) => shown(found),
        Err(why) => unknown(*why),
    }
}

/// The verdict the facts come to.
pub fn verdict_of(facts: &Facts, scrub: &Scrub) -> Verdict {
    verdict(&Evidence::of(facts, scrub))
}

/// The whole report as text, one fact to a line.
pub fn render(facts: &Facts, scrub: &Scrub) -> String {
    let client = facts.client.client();
    let mut lines = vec![format!("{APP_NAME} 진단 {}", facts.tool_version)];
    let mut number = 0;
    let mut line = |title: &str, body: String| {
        number += 1;
        lines.push(format!("{number:>2} {title}: {body}"));
    };

    let tables =
        format!("본섭 {}, 테섭 {}", facts.tables[0].label(&mfp_kms::LIVE), facts.tables[1].label(&mfp_kms::TEST));
    line("진단", format!("{} · {} {tables}", korean_time(facts.when), table_name(facts)));
    line("실행", format!("실행됨 · {}", or_unknown(&facts.started_from, |kind| format!("{} 디스크", kind.label()))));
    line("Windows", scrub.text(&facts.windows));
    line(
        "기기",
        format!("{} · {}코어 {}스레드 · 메모리 {}", scrub.text(&facts.cpu), facts.cores, facts.threads, gb(facts.ram)),
    );
    line(
        "권한",
        or_unknown(&facts.rights, |rights| {
            let yes = |is: bool| if is { "예" } else { "아니요" };
            let asks = match rights.asks_first {
                Some(true) => "켜짐",
                Some(false) => "꺼짐",
                None => "확인 불가",
            };
            format!("관리자 그룹 {} · 상승 {} · UAC {asks}", yes(rights.administrator), yes(rights.elevated))
        }),
    );
    line(
        "WebView2",
        or_unknown(&facts.webview, |found| match found {
            Some(webview) if webview.for_all => format!("{} (PC 전체)", scrub.text(&webview.version)),
            Some(webview) => format!("{} (사용자 단위)", scrub.text(&webview.version)),
            None => "없음".to_owned(),
        }),
    );
    line("연결", or_unknown(&facts.adapters, |adapters| adapters_line(adapters, scrub)));
    line("시스템 드라이브", or_unknown(&facts.system_drive, |volume| volume_line(volume, scrub)));
    line("설치", install_line(facts, scrub));
    line("게임 드라이브", game_drive_line(facts, scrub));
    line("게임", game_line(facts));
    line(
        "읽기",
        or_unknown(&facts.reads, |reads| {
            let seek =
                reads.seek_ms.map_or("임의 읽기 확인 불가".to_owned(), |ms| format!("임의 4 KB {ms:.1} ms"));
            format!("순차 {} {} · 다시 읽기 {} · {seek}", size(reads.bytes), speed(reads.first), speed(reads.again))
        }),
    );
    line(
        "받기",
        match &facts.shelf {
            Ok(shelf) => or_unknown(&shelf.sample, |(bytes, seconds)| {
                format!("{} {} (연결 시간 포함)", size(*bytes), speed(*bytes as f64 / seconds.max(1e-6)))
            }),
            Err(why) => unknown(*why),
        },
    );
    line(
        "패치",
        or_unknown(&facts.shelf, |shelf| {
            let named = |patch: &crate::facts::PatchOnShelf| {
                let originals = patch
                    .originals
                    .map_or("원본 표 없음".to_owned(), |files| format!("원본 표 {}개", count(files as u64)));
                format!("{} {} · {originals}", patch.file_name, size(patch.size))
            };
            let patch = match &shelf.patch {
                Some(patch) if shelf.patch_is_next_main => format!("다음 본 패치 {}", named(patch)),
                Some(patch) => format!("다음 본 패치 없음 · 최신 마이너 패치 {}", named(patch)),
                None => "다음 본 패치 없음".to_owned(),
            };
            format!("서버 최신 {} · {patch}", shelf.newest.label(client))
        }),
    );
    line("대조(빠름)", or_unknown(&facts.quick, |quick| comparison_line(quick, facts, false)));
    line(
        "대조(전체)",
        match &facts.full {
            // The quick comparison found nothing too long to read, and so was the full one.
            Asked::Done(full) if facts.quick.as_ref().ok() == Some(full) => {
                "빠른 대조와 같음(읽지 않은 파일 없음)".to_owned()
            }
            Asked::Done(full) => comparison_line(full, facts, true),
            Asked::Declined | Asked::NotPossible => "건너뜀".to_owned(),
        },
    );
    line(
        "쓰기 시험",
        match facts.write_test {
            WriteTest::Passed => "성공".to_owned(),
            WriteTest::Failed(Some(code)) => format!("실패(오류 {code})"),
            WriteTest::Failed(None) => "실패".to_owned(),
            WriteTest::LeftBehind(code) => {
                format!("성공 · 시험 파일 삭제 실패{}", code.map_or(String::new(), |code| format!("(오류 {code})")))
            }
            WriteTest::Skipped => "건너뜀".to_owned(),
        },
    );
    let landing = match &facts.landing {
        Asked::Done(landing) => landing_lines(landing, scrub),
        Asked::Declined | Asked::NotPossible => vec!["건너뜀".to_owned()],
    };
    line("쓰기 위치", landing.join("\n"));
    let drivers = match facts.drivers_listed {
        Ok(()) => String::new(),
        Err(why) => format!(" · 드라이버 목록 {}", unknown(why)),
    };
    line(
        "매장 프로그램",
        format!("{}{drivers}", or_unknown(&facts.programs, |programs| programs_line(programs, scrub))),
    );
    line("판정", verdict_lines(&verdict_of(facts, scrub)).join("\n"));
    line("기록", wrote_lines(facts).join("\n"));
    lines.join("\n")
}

/// What the table of known file states is called: the one built into the program, or the
/// one that was handed to it as a file.
fn table_name(facts: &Facts) -> &'static str {
    if facts.table_given { "표 파일" } else { "내장 표" }
}

fn adapters_line(adapters: &[Adapter], scrub: &Scrub) -> String {
    if adapters.is_empty() {
        return "연결된 어댑터 없음".to_owned();
    }
    let shown = |adapter: &Adapter| {
        let kind = if adapter.wireless { "무선" } else { "유선" };
        let speed = adapter.speed.map_or("속도 확인 불가".to_owned(), line_speed);
        format!("{kind} {} · {speed}", scrub.text(&adapter.model))
    };
    adapters.iter().map(shown).collect::<Vec<_>>().join(" / ")
}

/// What is shown of a volume: letter, file system, kind of drive, how the disk is attached
/// and what it calls itself, whether it has to seek, the volume's name with its digits
/// covered, and the room on it.
fn volume_line(volume: &Volume, scrub: &Scrub) -> String {
    let mut parts = Vec::new();
    let letter = if volume.letter.is_empty() { "문자 없음".to_owned() } else { volume.letter.clone() };
    parts.push(format!("{letter} {}", or_unknown(&volume.file_system, |name| scrub.text(name))));
    parts.push(volume.kind.label().to_owned());
    match &volume.bus {
        Ok(bus) => parts.push(bus.label()),
        Err(Unknown::Skipped) => {}
        Err(why) => parts.push(format!("연결 방식 {}", unknown(*why))),
    }
    if !volume.model.is_empty() {
        parts.push(scrub.text(&volume.model));
    }
    match volume.seek_penalty {
        Ok(true) => parts.push("탐색 지연 있음".to_owned()),
        Ok(false) => parts.push("탐색 지연 없음".to_owned()),
        Err(Unknown::Skipped) => {}
        Err(why) => parts.push(format!("탐색 지연 {}", unknown(why))),
    }
    if !volume.label.is_empty() {
        parts.push(format!("이름 {}", scrub.label(&volume.label)));
    }
    if volume.read_only {
        parts.push("읽기 전용".to_owned());
    }
    parts.push(format!("전체 {}", or_unknown(&volume.total, |bytes| gb(*bytes))));
    parts.push(format!("여유 {}", or_unknown(&volume.free, |bytes| gb(*bytes))));
    parts.join(" · ")
}

fn install_line(facts: &Facts, scrub: &Scrub) -> String {
    let how = match facts.how {
        How::Given => "지정한 폴더",
        How::Registry => "레지스트리",
        How::UsualPlace => "기본 위치",
        How::DriveScan => "드라이브 훑기",
        How::Typed => "직접 입력",
        How::NotFound => "못 찾음",
    };
    let mut parts = vec![how.to_owned()];
    if let Some(install) = &facts.install {
        parts.push(scrub.path(install));
        parts.push(facts.client.client().title.to_owned());
    }
    let same = |recorded: &str| {
        let tidy = |path: &str| path.trim_end_matches(['\\', '/']).replace('/', "\\").to_lowercase();
        facts.install.as_ref().is_some_and(|install| tidy(&install.to_string_lossy()) == tidy(recorded))
    };
    parts.push(match &facts.registry.install_dir {
        None => "레지스트리 설치 경로 없음".to_owned(),
        Some(recorded) if same(recorded) => "레지스트리 설치 경로 있음(같음)".to_owned(),
        Some(recorded) => format!("레지스트리 설치 경로 있음(다름: {})", scrub.text(recorded)),
    });
    parts.push(match facts.registry.version_hint {
        Some(hint) => format!("레지스트리 버전 값 {hint}"),
        None => "레지스트리 버전 값 없음".to_owned(),
    });
    parts.join(" · ")
}

fn game_drive_line(facts: &Facts, scrub: &Scrub) -> String {
    if facts.install.is_none() {
        return unknown(Unknown::Skipped);
    }
    let system = facts.system_drive.as_ref().ok();
    or_unknown(&facts.game_drive, |volume| {
        let shown = volume_line(volume, scrub);
        let where_it_is = match system {
            Some(system) if !volume.letter.is_empty() && system.letter == volume.letter => {
                "시스템 드라이브와 같은 볼륨"
            }
            Some(system) => match (&system.disk, &volume.disk) {
                (Ok(system), Ok(game)) if system == game => "시스템 드라이브와 같은 디스크",
                (Ok(_), Ok(_)) => "시스템 드라이브와 다른 디스크",
                _ => "시스템 드라이브와 같은 디스크인지 확인 불가",
            },
            None => "시스템 드라이브와 같은 디스크인지 확인 불가",
        };
        format!("{shown} · {where_it_is}")
    })
}

fn game_line(facts: &Facts) -> String {
    let Some(game) = &facts.game else { return unknown(Unknown::Skipped) };
    let version = or_unknown(&game.file_version, |version| version.map(|part| part.to_string()).join("."));
    let log = game.patcher_log.as_deref().unwrap_or("없음");
    let files = or_unknown(&game.files, |(files, bytes)| format!("{}개 {}", count(*files), gb(*bytes)));
    let traces = if game.app_traces { "있음" } else { "없음" };
    format!("파일 버전 {version} · Patcher.log {log} · 파일 {files} · 작업 폴더 흔적 {traces}")
}

fn comparison_line(compared: &Comparison, facts: &Facts, all_names: bool) -> String {
    let client = facts.client.client();
    let mut parts = Vec::new();
    let source = |source: Source| match source {
        Source::Table => table_name(facts),
        Source::Server => "서버 표",
    };
    match (&compared.at, &compared.either[..], &compared.against) {
        (Some(at), _, against) => {
            let source = against.map_or("", |(_, from)| source(from));
            parts.push(format!("{}와 일치({source})", at.label(client)));
        }
        (None, [_, ..], _) => {
            let labels: Vec<String> = compared.either.iter().map(|state| state.label(client)).collect();
            parts.push(format!("{} 중 하나와 일치(구별할 파일을 읽지 못함)", labels.join(", ")));
        }
        (None, [], Some((nearest, from))) => {
            parts.push(format!("일치하는 버전 없음 · 가장 가까운 {}({}) 기준", nearest.label(client), source(*from)));
        }
        (None, [], None) => parts.push("대조할 표 없음".to_owned()),
    }
    // A version that came out after the table was made: nothing knows what its files are.
    let known_up_to = facts.tables[usize::from(facts.client == mfp_kms::Server::Test)];
    if let Ok(shelf) = &facts.shelf
        && shelf.newest.order() > known_up_to.order()
    {
        parts.push(format!("서버 최신 {}의 파일은 {}에 없음", shelf.newest.label(client), table_name(facts)));
    }
    let Tally { same, same_length, unread, differs, missing, unreadable } = &compared.tally;
    let named = |names: &[String], most: usize| {
        let shown = names.len().min(most);
        let more = if names.len() > shown { format!(" 외 {}개", names.len() - shown) } else { String::new() };
        if shown == 0 { String::new() } else { format!(": {}{more}", names[..shown].join(", ")) }
    };
    // Files that differ are named: all of them by the full comparison, the first of them by
    // the quick one. Files that are missing can be most of a folder, so only the full
    // comparison names any, and only the first.
    parts.push(format!("없음 {}{}", missing.len(), named(missing, if all_names { NAMES_SHOWN } else { 0 })));
    parts.push(format!("일치 {}", count(*same as u64)));
    if *same_length > 0 {
        parts.push(format!("크기만 확인 {}", count(*same_length as u64)));
    }
    if *unread > 0 {
        parts.push(format!("읽지 않음 {}", count(*unread as u64)));
    }
    parts.push(format!("불일치 {}{}", differs.len(), named(differs, if all_names { usize::MAX } else { NAMES_SHOWN })));
    if !unreadable.is_empty() {
        let codes: Vec<String> = unreadable.iter().filter_map(|(_, code)| *code).map(|code| code.to_string()).collect();
        let mut codes: Vec<String> = codes.into_iter().collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        codes.truncate(5);
        let why = if codes.is_empty() { String::new() } else { format!("(오류 {})", codes.join(", ")) };
        parts.push(format!("읽기 실패 {}{why}", unreadable.len()));
    }
    parts.push(format!("표에 없는 파일 {}", count(compared.not_in_table)));
    if compared.bytes_read > 0 {
        let rate = compared.bytes_read as f64 / compared.seconds.max(1e-6);
        parts.push(format!(
            "읽은 양 {} · {} · 평균 {}",
            size(compared.bytes_read),
            took(compared.seconds),
            speed(rate)
        ));
    }
    if compared.cut_short {
        parts.push("중단됨".to_owned());
    }
    parts.join(" · ")
}

/// A change in what is free, in megabytes with its sign.
fn change(before: Option<u64>, after: Option<u64>) -> String {
    match (before, after) {
        (Some(before), Some(after)) => {
            let megabytes = (after as f64 - before as f64) / (1u64 << 20) as f64;
            format!("{megabytes:+.0} MB")
        }
        _ => "확인 불가".to_owned(),
    }
}

fn landing_lines(landing: &Landing, scrub: &Scrub) -> Vec<String> {
    let rate = landing.written as f64 / landing.seconds.max(1e-6);
    let mut head = format!("{} 쓰기 · {} · {}", size(landing.written), took(landing.seconds), speed(rate));
    match landing.error {
        Some(0) => head.push_str(" · 쓰기 실패"),
        Some(code) => head.push_str(&format!(" · 쓰기 실패(오류 {code})")),
        None if landing.written < landing.wanted => head.push_str(" · 중단됨"),
        None => {}
    }
    head.push_str(if landing.deleted { " · 임시 파일 삭제됨" } else { " · 임시 파일 삭제 실패" });
    let mut lines = vec![head];
    for volume in &landing.volumes {
        let role = if volume.written_to { " (쓴 곳)" } else { "" };
        lines.push(format!(
            "    {}{role} 여유 {} · 쓰기 후 {} · 삭제 후 {}",
            scrub.text(&volume.letter),
            volume.before.map_or("확인 불가".to_owned(), gb),
            change(volume.before, volume.after_write),
            change(volume.before, volume.after_delete),
        ));
    }
    let [before, after_write, after_delete] = landing.memory;
    lines.push(format!(
        "    메모리 여유 {} · 쓰기 후 {} · 삭제 후 {}",
        before.map_or("확인 불가".to_owned(), gb),
        change(before, after_write),
        change(before, after_delete),
    ));
    lines
}

fn programs_line(programs: &Programs, scrub: &Scrub) -> String {
    let groups = [
        ("노하드", &programs.diskless),
        ("복구", &programs.restore),
        ("PC방 관리", &programs.seat),
        ("확인 필요", &programs.hints),
        ("참고", &programs.noted),
        ("게임 실행 중", &programs.game),
    ];
    let shown: Vec<String> = groups
        .into_iter()
        .filter(|(_, names)| !names.is_empty())
        .map(|(title, names)| {
            format!("{title} {}", names.iter().map(|name| scrub.text(name)).collect::<Vec<_>>().join(", "))
        })
        .collect();
    if shown.is_empty() { "없음".to_owned() } else { shown.join(" · ") }
}

/// The verdict as lines: each answer, and under it the facts it rests on.
pub fn verdict_lines(verdict: &Verdict) -> Vec<String> {
    let mut lines = Vec::new();
    let mut answer = |title: &str, answer: &str, facts: &[String]| {
        lines.push(format!("    {title}: {answer}"));
        lines.extend(facts.iter().map(|fact| format!("      - {fact}")));
    };
    answer("종류", verdict.kind.label(), &verdict.kind_facts);
    let patch = format!("{}(본 패치는 약 {} 쓰기)", verdict.patch_label(), gb(MAIN_PATCH_WRITES));
    answer("본 패치", &patch, &verdict.patch_facts);
    answer("재부팅 후", verdict.kept_label(), &verdict.kept_facts);
    // The first line carries on from the title of the report line.
    lines[0] = lines[0].trim_start().to_owned();
    lines
}

/// What the diagnostic wrote on this PC and what it asked of the network: everything, and
/// in so many words when it is nothing.
fn wrote_lines(facts: &Facts) -> Vec<String> {
    let mut lines = Vec::new();
    for wrote in &facts.wrote {
        lines.push(match wrote {
            Wrote::EmptyFile { removed: true } => "설치 폴더에 빈 파일 1개 생성 후 삭제".to_owned(),
            Wrote::EmptyFile { removed: false } => {
                "설치 폴더에 빈 파일 1개 생성 · 삭제 실패(.mfp-check-*.tmp 남음)".to_owned()
            }
            Wrote::ProbeFile { bytes, removed: true } => {
                format!("설치 폴더에 {} 임시 파일 1개 생성 후 삭제", size(*bytes))
            }
            Wrote::ProbeFile { bytes, removed: false } => {
                format!("설치 폴더에 {} 임시 파일 1개 생성 · 삭제 실패(.mfp-check-*.tmp 남음)", size(*bytes))
            }
            Wrote::Leftovers { removed, left: 0 } => format!("이전 실행이 남긴 임시 파일 {removed}개 삭제"),
            Wrote::Leftovers { removed, left } => {
                format!("이전 실행이 남긴 임시 파일 {removed}개 삭제 · {left}개 삭제 실패")
            }
            Wrote::Report { file_name } => format!("실행 파일 폴더에 진단 결과 저장({file_name})"),
        });
    }
    let wrote = if lines.is_empty() {
        "이 PC에 쓴 것 없음".to_owned()
    } else {
        format!("이 PC에 쓴 것 {}건", lines.len())
    };
    let mut out = vec![wrote];
    out.extend(lines.into_iter().map(|line| format!("      - {line}")));
    out.push("    그 밖의 파일·레지스트리·설정 변경 없음".to_owned());
    out.push(if facts.traffic.requests == 0 {
        "    통신 없음".to_owned()
    } else {
        format!(
            "    통신: 넥슨 패치 서버 요청 {}건 · 받은 양 {} · 보낸 정보 없음",
            facts.traffic.requests,
            size(facts.traffic.received)
        )
    });
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use mfp_kms::Server;

    use super::*;
    use crate::facts::{Bus, DriveKind, Game, Moved, PatchOnShelf, Reads, Registry, Rights, Shelf, Traffic, WebView};
    use crate::known::State;

    const GIB: u64 = 1 << 30;

    fn disk(letter: &str, bus: Bus, model: &str, label: &str, free_gib: u64) -> Volume {
        Volume {
            letter: letter.to_owned(),
            kind: DriveKind::Fixed,
            file_system: Ok("NTFS".to_owned()),
            label: label.to_owned(),
            read_only: false,
            bus: Ok(bus),
            model: model.to_owned(),
            seek_penalty: Ok(false),
            total: Ok(953 * GIB),
            free: Ok(free_gib * GIB),
            disk: Ok(0),
        }
    }

    fn compared(at: Option<State>, tally: Tally) -> Comparison {
        Comparison {
            at,
            either: Vec::new(),
            against: Some((State { version: 421, minor: 4 }, Source::Table)),
            tally,
            not_in_table: 27,
            bytes_read: 3 * GIB,
            seconds: 12.0,
            cut_short: false,
        }
    }

    /// A home PC on which everything was found.
    fn home() -> Facts {
        Facts {
            started_from: Ok(DriveKind::Fixed),
            windows: "Windows 11 Pro (26200) · 64비트".to_owned(),
            cpu: "11th Gen Intel(R) Core(TM) i7-11800H @ 2.30GHz".to_owned(),
            cores: 8,
            threads: 16,
            ram: 16 * GIB,
            rights: Ok(Rights { administrator: true, elevated: false, asks_first: Some(true) }),
            webview: Ok(Some(WebView { version: "131.0.2903.86".to_owned(), for_all: true })),
            adapters: Ok(vec![Adapter {
                model: "Realtek PCIe 2.5GbE Family Controller".to_owned(),
                wireless: false,
                speed: Some(1_000_000_000),
            }]),
            system_drive: Ok(disk("C:", Bus::NVME, "SAMSUNG MZVL21T0HCLR-00BL2", "Windows", 211)),
            how: How::Registry,
            install: Some(PathBuf::from(r"C:\Nexon\Maple")),
            registry: Registry { install_dir: Some(r"C:\Nexon\Maple".to_owned()), version_hint: Some(421) },
            game_drive: Ok(disk("C:", Bus::NVME, "SAMSUNG MZVL21T0HCLR-00BL2", "Windows", 211)),
            game: Some(Game {
                file_version: Ok([1, 421, 4, 0]),
                patcher_log: Some("00421_04".to_owned()),
                files: Ok((1239, 68 * GIB)),
                app_traces: false,
            }),
            reads: Ok(Reads { bytes: 256 << 20, first: 2.1e9, again: 2.2e9, seek_ms: Some(0.1) }),
            shelf: Ok(Shelf {
                newest: State { version: 421, minor: 4 },
                patch: Some(PatchOnShelf {
                    file_name: "03to04.patch".to_owned(),
                    size: 163_520_879,
                    originals: Some(0),
                }),
                patch_is_next_main: false,
                sample: Ok((8 << 20, 0.4)),
            }),
            quick: Ok(compared(
                Some(State { version: 421, minor: 4 }),
                Tally { same: 812, same_length: 400, ..Tally::default() },
            )),
            full: Asked::Declined,
            write_test: WriteTest::Passed,
            landing: Asked::Declined,
            programs: Ok(Programs::default()),
            drivers_listed: Ok(()),
            wrote: vec![Wrote::EmptyFile { removed: true }],
            traffic: Traffic { requests: 9, received: 8 << 20 },
            ..Facts::none_yet(1_795_675_260)
        }
    }

    fn nobody() -> Scrub {
        Scrub::new(Vec::new(), Vec::new())
    }

    #[test]
    fn report_of_a_home_pc_reads_line_by_line() {
        let report = render(&home(), &nobody());
        let lines: Vec<&str> = report.lines().collect();
        assert_eq!(lines[0], "Maple Fast Patch 진단 0.1.0");
        assert!(lines[1].starts_with(" 1 진단: 2026.11.26 15:41 · 내장 표 본섭 1.2.421"), "{}", lines[1]);
        assert_eq!(lines[2], " 2 실행: 실행됨 · 고정 디스크");
        assert_eq!(lines[3], " 3 Windows: Windows 11 Pro (26200) · 64비트");
        assert_eq!(
            lines[4],
            " 4 기기: 11th Gen Intel(R) Core(TM) i7-11800H @ 2.30GHz · 8코어 16스레드 · 메모리 16.0 GB"
        );
        assert_eq!(lines[5], " 5 권한: 관리자 그룹 예 · 상승 아니요 · UAC 켜짐");
        assert_eq!(lines[6], " 6 WebView2: 131.0.2903.86 (PC 전체)");
        assert_eq!(lines[7], " 7 연결: 유선 Realtek PCIe 2.5GbE Family Controller · 1 Gbps");
        assert_eq!(
            lines[8],
            " 8 시스템 드라이브: C: NTFS · 고정 · NVMe · SAMSUNG MZVL21T0HCLR-00BL2 · 탐색 지연 없음 · 이름 Windows · 전체 953.0 GB · 여유 211.0 GB"
        );
        assert_eq!(
            lines[9],
            r" 9 설치: 레지스트리 · C:\Nexon\Maple · 메이플스토리 · 레지스트리 설치 경로 있음(같음) · 레지스트리 버전 값 421"
        );
        assert!(lines[10].ends_with("· 시스템 드라이브와 같은 볼륨"), "{}", lines[10]);
        assert_eq!(
            lines[11],
            "11 게임: 파일 버전 1.421.4.0 · Patcher.log 00421_04 · 파일 1,239개 68.0 GB · 작업 폴더 흔적 없음"
        );
        assert_eq!(lines[12], "12 읽기: 순차 256 MB 2,003 MB/s · 다시 읽기 2,098 MB/s · 임의 4 KB 0.1 ms");
        assert_eq!(lines[13], "13 받기: 8 MB 20 MB/s (연결 시간 포함)");
        assert_eq!(
            lines[14],
            "14 패치: 서버 최신 1.2.421 (4) · 다음 본 패치 없음 · 최신 마이너 패치 03to04.patch 156 MB · 원본 표 0개"
        );
        assert_eq!(
            lines[15],
            "15 대조(빠름): 1.2.421 (4)와 일치(내장 표) · 없음 0 · 일치 812 · 크기만 확인 400 · 불일치 0 · 표에 없는 파일 27 · 읽은 양 3.0 GB · 12초 · 평균 256 MB/s"
        );
        assert_eq!(lines[16], "16 대조(전체): 건너뜀");
        assert_eq!(lines[17], "17 쓰기 시험: 성공");
        assert_eq!(lines[18], "18 쓰기 위치: 건너뜀");
        assert_eq!(lines[19], "19 매장 프로그램: 없음");
        assert_eq!(lines[20], "20 판정: 종류: 일반 PC로 보임");
        assert!(report.contains("    본 패치: 가능 예상(본 패치는 약 34.5 GB 쓰기)"), "{report}");
        assert!(report.contains("    재부팅 후: 유지 예상"), "{report}");
        // The last lines say what the diagnostic did here.
        let tail = &lines[lines.len() - 4..];
        assert_eq!(
            tail,
            [
                "21 기록: 이 PC에 쓴 것 1건",
                "      - 설치 폴더에 빈 파일 1개 생성 후 삭제",
                "    그 밖의 파일·레지스트리·설정 변경 없음",
                "    통신: 넥슨 패치 서버 요청 9건 · 받은 양 8 MB · 보낸 정보 없음",
            ]
        );
    }

    #[test]
    fn what_could_not_be_established_says_so_with_its_error_number_and_the_report_goes_on() {
        let locked_down = Facts {
            started_from: Err(Unknown::Windows(5)),
            rights: Err(Unknown::Windows(5)),
            webview: Err(Unknown::Windows(5)),
            adapters: Err(Unknown::Windows(1168)),
            system_drive: Err(Unknown::Windows(21)),
            programs: Err(Unknown::Windows(5)),
            drivers_listed: Err(Unknown::Windows(5)),
            shelf: Err(Unknown::NoConnection),
            ..Facts::none_yet(0)
        };
        let report = render(&locked_down, &nobody());
        for line in [
            " 2 실행: 실행됨 · 확인 불가(오류 5)",
            " 5 권한: 확인 불가(오류 5)",
            " 6 WebView2: 확인 불가(오류 5)",
            " 7 연결: 확인 불가(오류 1168)",
            " 8 시스템 드라이브: 확인 불가(오류 21)",
            " 9 설치: 못 찾음 · 레지스트리 설치 경로 없음 · 레지스트리 버전 값 없음",
            "10 게임 드라이브: 건너뜀",
            "11 게임: 건너뜀",
            "12 읽기: 건너뜀",
            "13 받기: 확인 불가(연결 실패)",
            "14 패치: 확인 불가(연결 실패)",
            "15 대조(빠름): 건너뜀",
            "17 쓰기 시험: 건너뜀",
            "19 매장 프로그램: 확인 불가(오류 5) · 드라이버 목록 확인 불가(오류 5)",
            "20 판정: 종류: 판단 불가",
            "21 기록: 이 PC에 쓴 것 없음",
            "    통신 없음",
        ] {
            assert!(report.lines().any(|found| found == line), "missing {line:?} in\n{report}");
        }
        // Parts of a drive that Windows would not tell of are named one by one.
        let half = Volume {
            bus: Err(Unknown::Windows(1)),
            seek_penalty: Err(Unknown::Windows(50)),
            free: Err(Unknown::Windows(5)),
            ..disk("D:", Bus::SATA, "", "GAME01", 0)
        };
        assert_eq!(
            volume_line(&half, &nobody()),
            "D: NTFS · 고정 · 연결 방식 확인 불가(오류 1) · 탐색 지연 확인 불가(오류 50) · 이름 GAME## · 전체 953.0 GB · 여유 확인 불가(오류 5)"
        );
    }

    #[test]
    fn seat_without_a_disk_is_reported_with_what_the_verdict_rests_on() {
        let seat = Facts {
            how: How::DriveScan,
            install: Some(PathBuf::from(r"D:\Nexon\Maple")),
            registry: Registry::default(),
            system_drive: Ok(Volume { total: Ok(50 * GIB), ..disk("C:", Bus::ISCSI, "GM VDISK", "", 31) }),
            game_drive: Ok(Volume { disk: Ok(1), ..disk("D:", Bus::ISCSI, "GM VDISK", "GAME01", 855) }),
            programs: Ok(Programs { seat: vec!["pmclient.exe".to_owned()], ..Programs::default() }),
            quick: Ok(compared(
                None,
                Tally {
                    same: 700,
                    differs: vec!["Canvas.dll".to_owned(), "MapleStory.exe".to_owned()],
                    ..Tally::default()
                },
            )),
            ..home()
        };
        let report = render(&seat, &nobody());
        assert!(
            report.contains(r" 9 설치: 드라이브 훑기 · D:\Nexon\Maple · 메이플스토리 · 레지스트리 설치 경로 없음"),
            "{report}"
        );
        assert!(
            report.contains("이름 GAME## · 전체 953.0 GB · 여유 855.0 GB · 시스템 드라이브와 다른 디스크"),
            "{report}"
        );
        assert!(report.contains("일치하는 버전 없음 · 가장 가까운 1.2.421 (4)(내장 표) 기준 · 없음 0 · 일치 700 · 불일치 2: Canvas.dll, MapleStory.exe"));
        assert!(report.contains("19 매장 프로그램: PC방 관리 pmclient.exe"), "{report}");
        assert!(report.contains("20 판정: 종류: 노하드 좌석으로 보임"), "{report}");
        assert!(report.contains("      - 시스템 드라이브 C: iSCSI 연결(네트워크 디스크)"), "{report}");
        assert!(report.contains("    본 패치: 어려움(본 패치는 약 34.5 GB 쓰기)"), "{report}");
        assert!(
            report.contains("      - 1.2.421 (4) 기준 다른 파일 2개 · 없는 파일 0개(패치가 원본 검사에서 멈춤)"),
            "{report}"
        );
        assert!(report.contains("    재부팅 후: 유지 안 됨 예상"), "{report}");
    }

    #[test]
    fn write_probe_is_reported_drive_by_drive() {
        let mib = 1u64 << 20;
        let landing = Landing {
            written: 512 * mib,
            wanted: 512 * mib,
            seconds: 2.0,
            error: None,
            volumes: vec![
                Moved {
                    letter: "C:".to_owned(),
                    written_to: false,
                    before: Some(31 * GIB),
                    after_write: Some(31 * GIB - 512 * mib),
                    after_delete: Some(31 * GIB - 500 * mib),
                },
                Moved {
                    letter: "D:".to_owned(),
                    written_to: true,
                    before: Some(855 * GIB),
                    after_write: Some(855 * GIB),
                    after_delete: None,
                },
            ],
            memory: [Some(9 * GIB), Some(9 * GIB - 30 * mib), Some(9 * GIB + 12 * mib)],
            deleted: true,
        };
        assert_eq!(
            landing_lines(&landing, &nobody()),
            [
                "512 MB 쓰기 · 2.0초 · 256 MB/s · 임시 파일 삭제됨",
                "    C: 여유 31.0 GB · 쓰기 후 -512 MB · 삭제 후 -500 MB",
                "    D: (쓴 곳) 여유 855.0 GB · 쓰기 후 +0 MB · 삭제 후 확인 불가",
                "    메모리 여유 9.0 GB · 쓰기 후 -30 MB · 삭제 후 +12 MB",
            ]
        );
        let cut =
            Landing { written: 100 * mib, error: Some(112), deleted: false, volumes: Vec::new(), ..landing.clone() };
        assert_eq!(
            landing_lines(&cut, &nobody())[0],
            "100 MB 쓰기 · 2.0초 · 50 MB/s · 쓰기 실패(오류 112) · 임시 파일 삭제 실패"
        );
        let stopped = Landing { written: 100 * mib, volumes: Vec::new(), ..landing };
        assert!(landing_lines(&stopped, &nobody())[0].ends_with("· 중단됨 · 임시 파일 삭제됨"));
    }

    #[test]
    fn everything_the_diagnostic_wrote_is_listed() {
        let facts = Facts {
            wrote: vec![
                Wrote::Leftovers { removed: 1, left: 0 },
                Wrote::EmptyFile { removed: true },
                Wrote::ProbeFile { bytes: 512 << 20, removed: true },
                Wrote::Report { file_name: "mfp-check-20261126-1541.txt".to_owned() },
            ],
            ..Facts::none_yet(0)
        };
        assert_eq!(
            wrote_lines(&facts),
            [
                "이 PC에 쓴 것 4건",
                "      - 이전 실행이 남긴 임시 파일 1개 삭제",
                "      - 설치 폴더에 빈 파일 1개 생성 후 삭제",
                "      - 설치 폴더에 512 MB 임시 파일 1개 생성 후 삭제",
                "      - 실행 파일 폴더에 진단 결과 저장(mfp-check-20261126-1541.txt)",
                "    그 밖의 파일·레지스트리·설정 변경 없음",
                "    통신 없음",
            ]
        );
        let failed = Facts { wrote: vec![Wrote::ProbeFile { bytes: 512 << 20, removed: false }], ..Facts::none_yet(0) };
        assert!(wrote_lines(&failed)[1].contains("삭제 실패"));
    }

    #[test]
    fn long_list_of_differing_files_is_cut_in_the_quick_comparison_and_whole_in_the_full_one() {
        let names: Vec<String> = (0..25).map(|at| format!("Data\\Map_{at:03}.wz")).collect();
        let many = compared(None, Tally { same: 5, differs: names.clone(), ..Tally::default() });
        let facts = home();
        let quick = comparison_line(&many, &facts, false);
        assert!(quick.contains("불일치 25: Data\\Map_000.wz") && quick.contains("Data\\Map_019.wz 외 5개"), "{quick}");
        assert!(!quick.contains("Map_020"));
        let full = comparison_line(&many, &facts, true);
        assert!(full.contains("Map_024.wz") && !full.contains("외 5개"), "{full}");
        // Missing files are counted by the quick comparison, and the full one names the first.
        let gone = compared(None, Tally { same: 5, missing: names, ..Tally::default() });
        let quick = comparison_line(&gone, &facts, false);
        assert!(quick.contains("· 없음 25 · 일치 5 · 불일치 0 ·"), "{quick}");
        let full = comparison_line(&gone, &facts, true);
        assert!(
            full.contains("없음 25: Data\\Map_000.wz") && full.contains("Data\\Map_019.wz 외 5개 · 일치 5"),
            "{full}"
        );
        // Files that could not be read are counted, with the error numbers they failed with.
        let locked = compared(
            Some(State { version: 421, minor: 4 }),
            Tally {
                same: 5,
                unreadable: vec![
                    ("a.wz".to_owned(), Some(32)),
                    ("b.wz".to_owned(), Some(32)),
                    ("c.wz".to_owned(), Some(5)),
                ],
                ..Tally::default()
            },
        );
        assert!(comparison_line(&locked, &facts, false).contains("읽기 실패 3(오류 32, 5)"));
    }

    /// The hard rule: nothing that names the PC or its user comes out of the report, whatever
    /// Windows and the folder hand in.
    #[test]
    fn nothing_that_names_the_pc_or_its_user_comes_out_of_the_report() {
        const USER: &str = "Minsu";
        const COMPUTER: &str = "DESKTOP-7QK2M";
        const HOME: &str = r"C:\Users\Minsu";
        const CARD: &str = "00-1A-2B-3C-4D-5E";
        const CARD_WITH_COLONS: &str = "00:1a:2b:3c:4d:5e";
        const ADDRESS: &str = "192.168.0.15";
        const ADDRESS_V6: &str = "fe80::1ff:fe23:4567:890a";
        let scrub = Scrub::new([USER.to_owned(), COMPUTER.to_owned()], [HOME.to_owned()]);

        let hostile = |what: &str| {
            format!("{what} {USER} {COMPUTER} {HOME}\\secret {CARD} {CARD_WITH_COLONS} {ADDRESS} {ADDRESS_V6}\r\n")
        };
        let hostile_volume = |letter: &str| Volume {
            file_system: Ok(hostile("NTFS")),
            label: format!("{COMPUTER}_{USER}_7391"),
            model: hostile("Disk"),
            ..disk(letter, Bus::SATA, "", "", 100)
        };
        let names = |what: &str| vec![format!("{USER}-{what}.exe"), format!("{COMPUTER}{what}.sys")];
        let facts = Facts {
            windows: hostile("Windows"),
            cpu: hostile("CPU"),
            webview: Ok(Some(WebView { version: hostile("1.2.3.4"), for_all: false })),
            adapters: Ok(vec![Adapter { model: hostile("Realtek"), wireless: false, speed: None }]),
            system_drive: Ok(hostile_volume("C:")),
            how: How::Typed,
            install: Some(PathBuf::from(format!(r"{HOME}\Games\{COMPUTER}\Maple"))),
            registry: Registry { install_dir: Some(format!(r"c:\users\{USER}\Other\Maple")), version_hint: Some(421) },
            game_drive: Ok(Volume { disk: Ok(3), ..hostile_volume("D:") }),
            programs: Ok(Programs {
                diskless: names("pfgame"),
                restore: names("restore"),
                seat: names("pmclient"),
                hints: names("cache"),
                noted: names("wireguard"),
                game: names("MapleStory"),
            }),
            landing: Asked::Done(Landing {
                written: 1 << 20,
                wanted: 1 << 20,
                seconds: 1.0,
                error: None,
                volumes: vec![Moved {
                    letter: format!(r"\\{COMPUTER}\{USER}"),
                    written_to: true,
                    before: Some(1),
                    after_write: Some(1),
                    after_delete: Some(1),
                }],
                memory: [None; 3],
                deleted: true,
            }),
            ..home()
        };
        // Every answer of the verdict quotes names of programs somewhere; all kinds of
        // verdict are gone through by taking the evidence away piece by piece.
        let mut reports = vec![render(&facts, &scrub)];
        let mut fewer = facts.clone();
        for step in 0..4 {
            if let Ok(programs) = fewer.programs.as_mut() {
                match step {
                    0 => programs.diskless.clear(),
                    1 => programs.restore.clear(),
                    2 => programs.seat.clear(),
                    _ => programs.hints.clear(),
                }
            }
            reports.push(render(&fewer, &scrub));
        }
        for report in &reports {
            let lower = report.to_lowercase();
            // The last one is the number in the names of the volumes.
            for secret in [USER, COMPUTER, HOME, CARD, CARD_WITH_COLONS, ADDRESS, ADDRESS_V6, "7391"] {
                assert!(!lower.contains(&secret.to_lowercase()), "{secret:?} came out in\n{report}");
            }
            assert!(
                !report.contains(r"C:\Users") && !lower.contains(r"c:\users"),
                "a user's folder came out in\n{report}"
            );
            // What Windows handed in cannot break the report's lines either.
            assert!(report.lines().all(|line| !line.is_empty()), "{report}");
        }
        assert!(reports[0].contains(r"%USERPROFILE%\Games\***\Maple"), "{}", reports[0]);
        assert!(reports[0].contains(r"레지스트리 설치 경로 있음(다름: %USERPROFILE%\Other\Maple)"), "{}", reports[0]);
        assert!(reports[0].contains("이름 ***_***_####"), "{}", reports[0]);
    }

    #[test]
    fn test_world_is_named_as_the_test_world() {
        let facts = Facts {
            client: Server::Test,
            install: Some(PathBuf::from(r"C:\Nexon\MapleT")),
            registry: Registry::default(),
            quick: Ok(Comparison {
                at: Some(State { version: 1208, minor: 2 }),
                ..compared(None, Tally { same: 9, ..Tally::default() })
            }),
            shelf: Ok(Shelf {
                newest: State { version: 1208, minor: 2 },
                patch: None,
                patch_is_next_main: false,
                sample: Err(Unknown::Http(503)),
            }),
            ..home()
        };
        let report = render(&facts, &nobody());
        assert!(report.contains(r"C:\Nexon\MapleT · 메이플스토리 테스트월드"), "{report}");
        assert!(report.contains("13 받기: 확인 불가(HTTP 503)"), "{report}");
        assert!(report.contains("14 패치: 서버 최신 1.2.208 (2) · 다음 본 패치 없음"), "{report}");
        assert!(report.contains("15 대조(빠름): 1.2.208 (2)와 일치"), "{report}");
    }

    /// A minor patch came out after the table was made. A folder that has it differs from
    /// everything the table knows, and the report does not call that a fault of the folder.
    #[test]
    fn version_newer_than_the_table_is_not_held_against_the_folder() {
        let newer = State { version: 421, minor: 9 };
        let facts = Facts {
            shelf: Ok(Shelf { newest: newer, patch: None, patch_is_next_main: false, sample: Ok((8 << 20, 0.4)) }),
            quick: Ok(compared(
                None,
                Tally { same: 800, differs: vec!["MapleStory.exe".to_owned()], ..Tally::default() },
            )),
            ..home()
        };
        let report = render(&facts, &nobody());
        assert!(
            report.contains(
                "15 대조(빠름): 일치하는 버전 없음 · 가장 가까운 1.2.421 (4)(내장 표) 기준 · 서버 최신 1.2.421 (9)의 파일은 내장 표에 없음 · 없음 0 · 일치 800 · 불일치 1: MapleStory.exe"
            ),
            "{report}"
        );
        assert!(report.contains("    본 패치: 판단 불가(본 패치는 약 34.5 GB 쓰기)"), "{report}");
        assert!(
            report.contains(
                "      - 서버 최신 1.2.421 (9)의 파일은 이 도구의 표에 없음 · 1.2.421 (4) 기준 다른 파일 1개 · 없는 파일 0개(새 버전이어서일 수 있음)"
            ),
            "{report}"
        );
        // With the server at the version the table knows, the same folder is at fault.
        let known = Facts {
            shelf: Ok(Shelf {
                newest: State { version: 421, minor: 4 },
                patch: None,
                patch_is_next_main: false,
                sample: Err(Unknown::NoAnswer),
            }),
            ..facts.clone()
        };
        assert!(render(&known, &nobody()).contains("    본 패치: 어려움(본 패치는 약 34.5 GB 쓰기)"));
        // And without a server to ask, nothing is said either way.
        let unasked = Facts { shelf: Err(Unknown::NoConnection), ..facts };
        let report = render(&unasked, &nobody());
        assert!(report.contains("    본 패치: 판단 불가(본 패치는 약 34.5 GB 쓰기)"), "{report}");
        assert!(report.contains("서버 최신 버전을 확인하지 못해 새 버전이어서인지 판단 불가"), "{report}");
    }
}
