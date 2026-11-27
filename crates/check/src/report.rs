//! Writes the report: what was found, group by group, and what the check itself did on
//! the PC.
//!
//! The report is made to be passed on, so this is the one place where text from Windows or
//! from a folder turns into text that is shown, and every such piece goes through the
//! [`Scrub`] here. What must never be in a report is not in [`Facts`] to begin with: no
//! serial number, no address of a network card or of the PC, no list of what runs on it.
//!
//! A report is plain text in columns. Its first line names the program it is out of, with
//! its version, and when the check ran. Under it stand the groups, each under a heading in
//! brackets and after an empty line. A group is rows: what a row is about, and from one
//! column on, the same in every group, what is said of it. What goes on in a further line
//! begins at that column too. No line is wider than 76 columns, where a letter of Korean
//! takes two.
//!
//! The rows are fixed. Most are there in every report, and say `없음`, `확인 실패` or
//! `미실행` where there is nothing else to say; the others are there when the step they
//! are of was run.

use std::collections::BTreeSet;

use crate::facts::{
    Asked, DriveKind, Fact, Facts, Halt, Landing, OWN_FOLDER, Place, Step, Unknown, Volume, WriteLimit, WriteTest,
    Wrote,
};
use crate::known::{Source, State};
use crate::privacy::Scrub;
use crate::units::{
    count, gb, gigabytes, korean_time, line_speed, megabytes_a_second, milliseconds, size, speed, took,
};
use crate::verdict::{Evidence, Kind, Outlook, ROOM_NEEDED, Verdict, dip, gone_by, verdict};

/// What the app is called.
const APP_NAME: &str = "Maple Fast Patch";
/// The widest a line of the report is, in columns. A letter of Korean takes two.
const WIDTH: usize = 76;
/// The column what is said of a row begins at, counted from nought: the same in every group.
const VALUES_AT: usize = 18;
/// The columns there are for what is said of a row.
const ROOM: usize = WIDTH - VALUES_AT;
/// The most names of files a row lists. What is left out is counted.
const NAMES_SHOWN: usize = 20;
/// The most lines a row of the analysis takes.
const GROUND_LINES: usize = 2;
/// Half a megabyte: less than this is nothing, as megabytes are written.
const HALF_A_MEGABYTE: u64 = 1 << 19;

/// Which program a report is out of. Its first line names the program and its version.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Head {
    /// The check's own program, by the version of the check.
    #[default]
    Check,
    /// The app, by this version of it.
    App(String),
}

/// The verdict the facts come to.
pub fn verdict_of(facts: &Facts, scrub: &Scrub) -> Verdict {
    verdict(&Evidence::of(facts, scrub))
}

/// The whole report as text, headed by the program `head` names.
pub fn render(facts: &Facts, scrub: &Scrub, head: &Head) -> String {
    let judged = verdict_of(facts, scrub);
    let when = korean_time(facts.when);
    let mut lines = vec![match head {
        Head::Check => format!("{APP_NAME} 검사 {} · {when}", facts.tool_version),
        Head::App(version) => format!("{APP_NAME} {version} · {when}"),
    }];
    group(&mut lines, "검사 요약", summary(facts, scrub, &judged));
    group(&mut lines, "PC 환경", pc(facts, scrub, &judged));
    group(&mut lines, "디스크 환경", disks(facts, scrub));
    group(&mut lines, "게임 설치 상태", install(facts, scrub));
    group(&mut lines, "게임 파일 검사", files(facts, scrub));
    group(&mut lines, "네트워크 환경", network(facts));
    group(&mut lines, "성능 측정", speeds(facts));
    group(&mut lines, "패치 환경 검사", writes(facts, scrub));
    group(&mut lines, "환경 분석", analysis(&judged));
    group(&mut lines, "검사 기록", record(facts, scrub));
    lines.join("\n")
}

/// A row of the report: what it is about, and what is said of it, line by line.
struct Row {
    name: String,
    lines: Vec<String>,
}

impl Row {
    /// A row that says one thing.
    fn of(name: &str, said: impl Into<String>) -> Row {
        Row::with(name, &[said.into()])
    }

    /// A row that says several things side by side.
    fn with(name: &str, said: &[String]) -> Row {
        Row { name: name.to_owned(), lines: lay(said) }
    }

    /// A row that says one thing in one line, whatever its length: a path.
    fn whole(name: &str, said: &str) -> Row {
        Row { name: name.to_owned(), lines: vec![middle(said, ROOM)] }
    }

    /// The row with more under what it says already.
    fn under(mut self, said: &[String]) -> Row {
        self.lines.extend(lay(said));
        self
    }
}

/// Adds a group to the lines of a report: an empty line, its heading, and its rows. A
/// group without a row is left out.
fn group(lines: &mut Vec<String>, title: &str, rows: Vec<Row>) {
    if rows.is_empty() {
        return;
    }
    lines.push(String::new());
    lines.push(format!("[{title}]"));
    for row in rows {
        // Two columns stay empty between the longest name and what is said.
        let name = middle(&row.name, VALUES_AT - 2);
        for (at, line) in row.lines.iter().enumerate() {
            let name = if at == 0 { name.as_str() } else { "" };
            lines.push(format!("{name}{}{line}", " ".repeat(VALUES_AT - width(name))));
        }
    }
}

/// How many columns a text takes, where every letter is as wide as the next but for those
/// of Korean, Chinese and Japanese, which take two.
fn width(text: &str) -> usize {
    text.chars().map(columns).sum()
}

fn columns(letter: char) -> usize {
    match letter as u32 {
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA960..=0xA97F
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6 => 2,
        _ => 1,
    }
}

/// A text in `room` columns: as it is where it fits, and else with `…` where its middle
/// was. Its end stays, from the last separator of a path on where that leaves room for a
/// beginning: the name of the file or folder.
fn middle(text: &str, room: usize) -> String {
    if width(text) <= room {
        return text.to_owned();
    }
    let letters: Vec<char> = text.chars().collect();
    let wide = |letters: &[char]| letters.iter().map(|letter| columns(*letter)).sum::<usize>();
    let last_part = letters.iter().rposition(|letter| matches!(letter, '\\' | '/'));
    let mut end = last_part.filter(|at| wide(&letters[*at..]) + 4 <= room).unwrap_or(letters.len());
    if end == letters.len() {
        // No part to keep whole: about two thirds of the room go to the end.
        let mut taken = 0;
        while end > 0 && taken + columns(letters[end - 1]) <= room.saturating_sub(1) * 2 / 3 {
            end -= 1;
            taken += columns(letters[end]);
        }
    }
    let mut used = wide(&letters[end..]) + 1;
    let mut start = String::new();
    for letter in &letters[..end] {
        if used + columns(*letter) > room {
            break;
        }
        start.push(*letter);
        used += columns(*letter);
    }
    format!("{start}…{}", letters[end..].iter().collect::<String>())
}

/// A text that is too long for a line, in lines: broken between its words, and a word that
/// is too long by itself cut in its middle.
fn fit(text: &str) -> Vec<String> {
    if width(text) <= ROOM {
        return vec![text.to_owned()];
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split(' ').filter(|word| !word.is_empty()) {
        let word = middle(word, ROOM);
        if line.is_empty() {
            line = word;
        } else if width(&line) + 1 + width(&word) <= ROOM {
            line.push(' ');
            line.push_str(&word);
        } else {
            lines.push(std::mem::replace(&mut line, word));
        }
    }
    lines.push(line);
    lines
}

/// What a row says, in lines no wider than there is room for: its values side by side
/// with ` · ` between them. A line ends where a value does, or after a comma inside one
/// that is a list.
fn lay(said: &[String]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for (at, value) in said.iter().filter(|value| !value.is_empty()).enumerate() {
        let parts: Vec<&str> = value.split(", ").collect();
        for (place, part) in parts.iter().enumerate() {
            // A part of a list keeps its comma, also at the end of a line.
            let part = if place + 1 < parts.len() { format!("{part},") } else { (*part).to_owned() };
            let before = match (at, place) {
                (0, 0) => "",
                (_, 0) => " · ",
                _ => " ",
            };
            if !line.is_empty() && width(&line) + width(before) + width(&part) <= ROOM {
                line.push_str(before);
                line.push_str(&part);
                continue;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            let mut broken = fit(&part);
            line = broken.pop().unwrap_or_default();
            lines.extend(broken);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// The facts an answer rests on, in the two lines a row of the analysis has at most. What
/// does not fit is counted at the end: `외 2개`.
fn grounds(facts: &[String]) -> Vec<String> {
    let lines = lay(facts);
    if lines.len() <= GROUND_LINES {
        return lines;
    }
    for kept in (1..facts.len()).rev() {
        let mut shown = facts[..kept].to_vec();
        shown[kept - 1].push_str(&format!(" 외 {}개", facts.len() - kept));
        let lines = lay(&shown);
        if lines.len() <= GROUND_LINES {
            return lines;
        }
    }
    lines.into_iter().take(GROUND_LINES).collect()
}

/// The names of files, as the lines under their count: the first of them, and how many
/// more there are.
fn listed<'a>(names: impl ExactSizeIterator<Item = &'a String>, scrub: &Scrub) -> Vec<String> {
    let more = names.len().saturating_sub(NAMES_SHOWN);
    let shown: Vec<String> = names.take(NAMES_SHOWN).map(|name| scrub.text(name)).collect();
    let mut said = shown.join(", ");
    if more > 0 {
        said.push_str(&format!(" 외 {}개", count(more as u64)));
    }
    lay(&[said])
}

/// Text from Windows with every run of blanks made one: disks and processors pad their names.
fn tidy(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Something about a volume with the volume's letter in front, where it has one:
/// `D: 여유 200.0 GB`.
fn on(letter: &str, said: &str) -> String {
    if letter.is_empty() { said.to_owned() } else { format!("{letter} {said}") }
}

/// What was measured, or why there is no figure: `미측정` for what was not measured at
/// all, which is not a failure.
fn measured(found: Fact<String>) -> String {
    match found {
        Ok(said) => said,
        Err(Unknown::Skipped) => "미측정".to_owned(),
        Err(why) => why.label(),
    }
}

/// The model of a processor without what its maker pads the name with: the marks, the
/// generation, the clock, and the words that say it is a processor.
fn cpu_name(raw: &str) -> String {
    let mut plain = raw.to_owned();
    for mark in ["(R)", "(r)", "(TM)", "(tm)"] {
        plain = plain.replace(mark, " ");
    }
    let plain = plain.split('@').next().unwrap_or_default();
    let mut words: Vec<&str> = plain.split_whitespace().collect();
    // `12th Gen` in front.
    let counted = |word: &str| word.len() > 2 && word[..word.len() - 2].chars().all(|digit| digit.is_ascii_digit());
    if words.len() > 2 && words[1].eq_ignore_ascii_case("gen") && counted(words[0]) {
        words.drain(..2);
    }
    words.retain(|word| {
        let lower = word.to_ascii_lowercase();
        !(lower == "cpu" || lower == "processor" || lower.ends_with("-core"))
    });
    words.join(" ")
}

/// What the answer about the patch comes to, read from the word a page shows for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Takes {
    Yes,
    /// Only the immediate way has the room.
    Immediately,
    No,
    Unknown,
}

impl Takes {
    fn of(judged: &Verdict) -> Takes {
        match judged.patch_plain.answer {
            "가능" => Takes::Yes,
            "즉시 패치로 가능" => Takes::Immediately,
            "불가" => Takes::No,
            _ => Takes::Unknown,
        }
    }
}

/// The verdict at a glance. Nothing is judged here: every row is the verdict's answer in
/// the words of the report, and the room is the figure the verdict holds the drive against.
fn summary(facts: &Facts, scrub: &Scrub, judged: &Verdict) -> Vec<Row> {
    let takes = Takes::of(judged);
    let overall = match (takes, judged.kept) {
        (Takes::No, _) => "패치 불가",
        (Takes::Unknown, _) => "확인 불가",
        (Takes::Yes, Outlook::Expected) => "정상",
        // It goes through, and there is something to know: it is gone at the next start,
        // nothing says whether it stays, or only the immediate way has the room.
        (Takes::Yes | Takes::Immediately, _) => "주의",
    };
    let possible = match takes {
        Takes::Yes => "가능".to_owned(),
        Takes::Immediately => "즉시 패치로 가능".to_owned(),
        Takes::No => format!("불가능 ({})", judged.patch_plain.reason),
        Takes::Unknown => "확인 불가".to_owned(),
    };
    let mut rows = vec![Row::of("전체 검사 결과", overall), Row::of("패치 가능 여부", possible)];
    if let Ok(Volume { letter, free: Ok(free), .. }) = &facts.game_drive {
        // What the usual way of patching needs: the engine's figure for the patch that is
        // waiting where the caller handed it in, and else the one the verdict goes by.
        let needed = facts.room.map_or(ROOM_NEEDED, |room| room.safe);
        let enough = if *free >= needed { "충분" } else { "부족" };
        let has = on(&scrub.text(letter), &format!("여유 {}", gb(*free)));
        rows.push(Row::of("패치 공간", format!("{enough} ({has} · 필요 {})", gb(needed))));
    }
    let kept = match judged.kept {
        Outlook::Expected => "가능",
        Outlook::Unlikely => "불가능 (재부팅·로그아웃 시 초기화)",
        Outlook::Unknown => "확인 불가",
    };
    rows.push(Row::of("패치 유지 여부", kept));
    if judged.kind == Kind::Diskless {
        rows.push(Row::of("자리 이동", "재패치 필요"));
    }
    rows
}

fn pc(facts: &Facts, scrub: &Scrub, judged: &Verdict) -> Vec<Row> {
    let failed = || Unknown::NoAnswer.label();
    let shop_program = facts.programs.as_ref().is_ok_and(|programs| !programs.seat.is_empty());
    let place = match judged.kind {
        Kind::Diskless => "PC방",
        _ if shop_program => "PC방",
        Kind::Unknown => "확인 불가",
        Kind::Ordinary | Kind::DiskWithRestore => "일반 PC",
    };
    let disk = match judged.kind {
        Kind::Diskless => "노하드 (서버 디스크 방식)",
        Kind::Ordinary => "PC 내장 디스크",
        Kind::DiskWithRestore => "복구 프로그램 사용",
        Kind::Unknown => "확인 불가",
    };
    let kept_on = match judged.kind {
        Kind::Diskless => "매장 서버 (네트워크)",
        Kind::Ordinary | Kind::DiskWithRestore => "이 PC의 디스크",
        Kind::Unknown => "확인 불가",
    };
    let windows = Some(tidy(&scrub.text(&facts.windows))).filter(|windows| !windows.is_empty());
    let mut cpu = vec![cpu_name(&scrub.text(&facts.cpu))];
    if facts.cores > 0 {
        cpu.push(format!("{}코어 {}스레드", facts.cores, facts.threads));
    }
    if cpu.iter().all(String::is_empty) {
        cpu = vec![failed()];
    }
    let rights = match &facts.rights {
        Ok(rights) => {
            let whose = if rights.elevated { "관리자" } else { "일반 사용자" };
            let asks = match rights.asks_first {
                Some(true) => "UAC 활성화",
                Some(false) => "UAC 비활성화",
                None => "UAC 확인 실패",
            };
            vec![whose.to_owned(), asks.to_owned()]
        }
        Err(why) => vec![why.label()],
    };
    let webview = match &facts.webview {
        Ok(Some(found)) => Some(tidy(&scrub.text(&found.version))).filter(|version| !version.is_empty()),
        Ok(None) => Some("없음".to_owned()),
        Err(why) => Some(why.label()),
    };
    vec![
        Row::of("사용 장소", place),
        Row::of("디스크 유형", disk),
        Row::of("게임 저장 위치", kept_on),
        Row::of("운영체제", windows.unwrap_or_else(failed)),
        Row::with("CPU", &cpu),
        Row::of("메모리", if facts.ram > 0 { gb(facts.ram) } else { failed() }),
        Row::with("실행 권한", &rights),
        Row::of("WebView2", webview.unwrap_or_else(failed)),
    ]
}

/// A volume in its two lines: what its disk is, and the room on it.
fn volume_row(volume: &Volume, role: &str, scrub: &Scrub) -> Row {
    let mut device = vec![tidy(&scrub.text(&volume.model))];
    match &volume.bus {
        Ok(bus) => device.push(bus.label()),
        Err(Unknown::Skipped) => {}
        Err(why) => device.push(format!("연결 방식 {}", why.label())),
    }
    match &volume.file_system {
        Ok(name) => device.push(tidy(&scrub.text(name))),
        Err(Unknown::Skipped) => {}
        Err(why) => device.push(format!("파일 시스템 {}", why.label())),
    }
    if volume.kind != DriveKind::Fixed {
        device.push(format!("{} 드라이브", volume.kind.label()));
    }
    if volume.read_only {
        device.push("읽기 전용".to_owned());
    }
    if device.iter().all(String::is_empty) {
        device = vec![format!("장치 정보 {}", Unknown::NoAnswer.label())];
    }
    let room = match (&volume.free, &volume.total) {
        (Ok(free), Ok(total)) => format!("여유 {} / {} GB", gigabytes(*free), gigabytes(*total)),
        (Err(why), Ok(total)) => format!("여유 {} / {} GB", why.label(), gigabytes(*total)),
        (Ok(free), Err(why)) => format!("여유 {} GB / 전체 {}", gigabytes(*free), why.label()),
        (Err(why), Err(_)) => format!("여유 {}", why.label()),
    };
    Row::with(&on(&scrub.text(&volume.letter), role), &device).under(&[room])
}

/// Which of the volumes share a disk, by the number Windows gives each disk:
/// `C:, D: 같은 디스크`, `G: 별도 디스크`.
fn layout(shown: &[(String, Fact<u32>)]) -> Vec<String> {
    let mut disks: Vec<(u32, Vec<&str>)> = Vec::new();
    for (letter, disk) in shown {
        let Ok(disk) = disk else { continue };
        match disks.iter_mut().find(|(number, _)| number == disk) {
            Some((_, letters)) => letters.push(letter),
            None => disks.push((*disk, vec![letter])),
        }
    }
    let unread: Vec<&str> = shown.iter().filter(|(_, disk)| disk.is_err()).map(|(letter, _)| letter.as_str()).collect();
    // One volume alone is held against nothing.
    if disks.iter().map(|(_, letters)| letters.len()).sum::<usize>() < 2 {
        return vec![Unknown::NoAnswer.label()];
    }
    let shared = disks.iter().filter(|(_, letters)| letters.len() > 1);
    let mut said: Vec<String> = shared.map(|(_, letters)| format!("{} 같은 디스크", letters.join(", "))).collect();
    let alone: Vec<&str> =
        disks.iter().filter(|(_, letters)| letters.len() == 1).map(|(_, letters)| letters[0]).collect();
    match alone[..] {
        [] => {}
        [only] => said.push(format!("{only} 별도 디스크")),
        _ => said.push(format!("{} 각각 별도 디스크", alone.join(", "))),
    }
    if !unread.is_empty() {
        said.push(format!("{} {}", unread.join(", "), Unknown::NoAnswer.label()));
    }
    said
}

/// Every volume: the one Windows is on, the game's, and the others by their letters.
fn disks(facts: &Facts, scrub: &Scrub) -> Vec<Row> {
    let mut rows = Vec::new();
    // The volumes that have a row and a letter, each with the number of its disk.
    let mut shown: Vec<(String, Fact<u32>)> = Vec::new();
    let mut add = |volume: &Volume, role: &str, rows: &mut Vec<Row>| {
        rows.push(volume_row(volume, role, scrub));
        if !volume.letter.is_empty() {
            shown.push((scrub.text(&volume.letter), volume.disk));
        }
    };
    let (system, game) = (&facts.system_drive, &facts.game_drive);
    let one =
        matches!((system, game), (Ok(system), Ok(game)) if !game.letter.is_empty() && system.letter == game.letter);
    match system {
        Ok(volume) => add(volume, if one { "시스템·게임" } else { "시스템" }, &mut rows),
        Err(Unknown::Skipped) => {}
        Err(why) => rows.push(Row::of("시스템", why.label())),
    }
    if facts.install.is_some() && !one {
        match game {
            Ok(volume) => add(volume, "게임", &mut rows),
            Err(Unknown::Skipped) => {}
            Err(why) => rows.push(Row::of("게임", why.label())),
        }
    }
    let mut others: Vec<&Volume> = facts.other_volumes.iter().collect();
    others.sort_by(|one, other| one.letter.cmp(&other.letter));
    for volume in others {
        add(volume, "기타", &mut rows);
    }
    if shown.len() > 1 {
        rows.push(Row::with("디스크 구성", &layout(&shown)));
    }
    rows
}

/// What is installed: which game, where, at which version, and how much of it there is.
fn install(facts: &Facts, scrub: &Scrub) -> Vec<Row> {
    let client = facts.client.client();
    let mut rows = vec![Row::of("게임 이름", client.title)];
    let Some(folder) = &facts.install else {
        rows.push(Row::of("설치 경로", "없음"));
        return rows;
    };
    rows.push(Row::whole("설치 경로", &scrub.path(folder)));
    let game = facts.game.as_ref();
    // The version is the one the folder's files are at, by the comparison that is gone
    // by. Where they fit none, as with a file missing, it is the one the game's own
    // patcher logged last. Without such a line, the version the game's program carries is
    // all there is, and that is shown as the four numbers it is.
    let compared = gone_by(facts);
    let either = compared.map_or(&[][..], |compared| &compared.either[..]);
    let logged = game.and_then(|game| game.patcher_log.as_deref()).and_then(State::logged);
    let at = compared.and_then(|compared| compared.at).or(logged.filter(|_| either.is_empty()));
    let carried = game.and_then(|game| game.file_version.ok());
    let carried = carried.map(|version| format!("파일 버전 {}", version.map(|part| part.to_string()).join(".")));
    let current = match (at, either, compared) {
        (Some(at), ..) => vec![at.label(client)],
        (None, [_, ..], _) => vec![labels(either, facts)],
        (None, [], Some(_)) => std::iter::once("일치 버전 없음".to_owned()).chain(carried).collect(),
        (None, [], None) => vec![carried.unwrap_or_else(|| Unknown::NoAnswer.label())],
    };
    rows.push(Row::with("현재 게임 버전", &current));
    let newest = match &facts.shelf {
        Ok(shelf) => shelf.newest.label(client),
        // The server was not asked, and whoever started the check did not say either.
        Err(Unknown::Skipped) => "미확인".to_owned(),
        Err(why) => why.label(),
    };
    rows.push(Row::of("최신 게임 버전", newest));
    // What the registry and the game's own patcher say, where they say another version.
    if let Some(at) = at {
        if let Some(hint) = facts.registry.version_hint
            && hint != at.version
            && format!("1.2.{hint}") != client.version_label(at.version)
        {
            rows.push(Row::of("등록 버전", hint.to_string()));
        }
        if let Some(log) = game.and_then(|game| game.patcher_log.as_deref()) {
            match State::logged(log) {
                Some(logged) if logged.version == at.version && (at.minor == 0 || logged.minor == at.minor) => {}
                Some(logged) => rows.push(Row::of("패치 기록 버전", logged.label(client))),
                None => rows.push(Row::of("패치 기록 버전", tidy(&scrub.text(log)))),
            }
        }
    }
    let (bytes, files) = match game.map(|game| &game.files) {
        Some(Ok((files, bytes))) => (size(*bytes), format!("{}개", count(*files))),
        Some(Err(why)) => (why.label(), why.label()),
        None => (Unknown::NoAnswer.label(), Unknown::NoAnswer.label()),
    };
    rows.push(Row::of("설치 용량", bytes));
    rows.push(Row::of("전체 파일 개수", files));
    rows
}

/// Several states a folder could be at, as one value.
fn labels(states: &[State], facts: &Facts) -> String {
    states.iter().map(|state| state.label(facts.client.client())).collect::<Vec<_>>().join(", ")
}

/// A count of files with their names under it, or that there are none.
fn counted(name: &str, files: &[String], scrub: &Scrub) -> Row {
    if files.is_empty() {
        return Row::of(name, "없음");
    }
    let mut row = Row::of(name, format!("{}개", count(files.len() as u64)));
    row.lines.extend(listed(files.iter(), scrub));
    row
}

/// How the folder's files stand against what Nexon published, by the strongest comparison
/// that was run, also where it was stopped on its way.
fn files(facts: &Facts, scrub: &Scrub) -> Vec<Row> {
    let client = facts.client.client();
    let (basis, compared) = match (&facts.full, &facts.quick, &facts.sizes) {
        (Asked::Done(full), ..) => ("파일 내용 비교 (전체)", full),
        (_, Ok(quick), _) => ("파일 내용 비교 (일부)", quick),
        (_, _, Ok(sizes)) => ("파일 크기 비교", sizes),
        // There was no folder to look at.
        (_, _, Err(Unknown::Skipped)) => return vec![Row::of("검사 기준", "미실행")],
        (_, _, Err(why)) => return vec![Row::of("검사 기준", why.label())],
    };
    // A list other than the one built into the program is said to be one.
    let list = match compared.against {
        Some((_, Source::Server)) => "서버 목록",
        _ if facts.table_given => "지정 목록",
        _ => "",
    };
    let against = match (&compared.at, &compared.either[..], &compared.against) {
        (Some(at), _, _) => vec![at.label(client)],
        (None, [_, ..], _) => vec![labels(&compared.either, facts)],
        (None, [], Some((nearest, _))) => vec![nearest.label(client), "일치 버전 없음".to_owned()],
        (None, [], None) => vec!["없음".to_owned()],
    };
    let tally = &compared.tally;
    // Files that were not read are sound by their length, or by their being there.
    let by_size = tally.same_length + tally.unread;
    let sound = count((tally.same + by_size) as u64);
    let sound = if compared.by_length || by_size == 0 {
        format!("{sound}개")
    } else {
        format!("{sound}개 (내용 {}개 · 크기만 {}개)", count(tally.same as u64), count(by_size as u64))
    };
    let mut rows = vec![
        Row::with("검사 기준", &[basis.to_owned(), list.to_owned()]),
        Row::with("검사 대상 버전", &against),
        Row::of("정상 파일", sound),
        counted("변경 파일", &tally.differs, scrub),
        counted("누락 파일", &tally.missing, scrub),
    ];
    if !tally.unreadable.is_empty() {
        let codes: BTreeSet<i32> = tally.unreadable.iter().filter_map(|(_, code)| *code).collect();
        let codes: Vec<String> = codes.iter().take(5).map(i32::to_string).collect();
        let why = if codes.is_empty() { String::new() } else { format!(" (오류 {})", codes.join(", ")) };
        let mut row = Row::of("읽기 실패 파일", format!("{}개{why}", count(tally.unreadable.len() as u64)));
        row.lines.extend(listed(tally.unreadable.iter().map(|(name, _)| name), scrub));
        rows.push(row);
    }
    rows.push(Row::of("검사 제외 파일", format!("{}개", count(compared.not_in_table))));
    if !compared.by_length {
        let stopped = if compared.cut_short { "중단됨" } else { "" };
        rows.push(Row::with("검사 시간", &[took(compared.seconds), size(compared.bytes_read), stopped.to_owned()]));
    }
    rows
}

fn network(facts: &Facts) -> Vec<Row> {
    let mut rows = Vec::new();
    match &facts.adapters {
        Ok(adapters) if adapters.is_empty() => rows.push(Row::of("연결 방식", "없음")),
        Ok(adapters) => {
            let kind = |wireless: bool| if wireless { "무선 Wi-Fi" } else { "유선 LAN" };
            let kinds: Vec<String> = adapters.iter().map(|adapter| kind(adapter.wireless).to_owned()).collect();
            let unread = || Unknown::NoAnswer.label();
            let speeds: Vec<String> =
                adapters.iter().map(|adapter| adapter.speed.map_or_else(unread, line_speed)).collect();
            rows.push(Row::with("연결 방식", &kinds));
            rows.push(Row::with("네트워크 속도", &speeds));
        }
        Err(why) => rows.push(Row::of("연결 방식", why.label())),
    }
    // Both are of the way to Nexon's server, and neither is there where it was not asked.
    let (sample, round_trip) = match &facts.shelf {
        Ok(shelf) => (shelf.sample, shelf.round_trip),
        Err(why) => (Err(*why), Err(*why)),
    };
    let sample = sample.map(|(bytes, seconds)| speed(bytes as f64 / seconds.max(1e-6)));
    rows.push(Row::of("다운로드 속도", measured(sample)));
    rows.push(Row::of("응답 지연 시간", measured(round_trip.map(milliseconds))));
    rows
}

fn speeds(facts: &Facts) -> Vec<Row> {
    match &facts.reads {
        Ok(reads) => {
            vec![Row::of("디스크 읽기 속도", speed(reads.first)), Row::of("반복 읽기 속도", speed(reads.again))]
        }
        Err(why) => vec![Row::of("디스크 읽기 속도", measured(Err(*why)))],
    }
}

/// Why the write limit test was refused, in the words of the report: the volume has no
/// room for it, or took no write, or too little memory is available. `None` for a test
/// that was run, or that was only not asked for.
pub fn limit_not_run(limit: &WriteLimit, scrub: &Scrub) -> Option<String> {
    Some(match limit {
        WriteLimit::Done(_) | WriteLimit::Declined | WriteLimit::NotPossible => return None,
        WriteLimit::NoRoom { volume, free: Some(free), writes, floor } => {
            let needed = gb(writes.saturating_add(*floor));
            format!("{} / 필요 {needed}", on(&scrub.text(volume), &format!("여유 {}", gb(*free))))
        }
        WriteLimit::NoRoom { volume, free: None, .. } => {
            on(&scrub.text(volume), &format!("여유 {}", Unknown::NoAnswer.label()))
        }
        WriteLimit::NoPlace { volume, error: Some(code) } => {
            on(&scrub.text(volume), &format!("쓰기 실패, 오류 {code}"))
        }
        WriteLimit::NoPlace { volume, error: None } => on(&scrub.text(volume), "쓰기 가능한 고정 볼륨 아님"),
        WriteLimit::NoMemory { available: Some(available), wanted } => {
            format!("메모리 {} / 필요 {}", gb(*available), gb(*wanted))
        }
        WriteLimit::NoMemory { available: None, .. } => format!("메모리 {}", Unknown::NoAnswer.label()),
    })
}

/// How the speed of a write changed on its way, where its last step was written at a
/// speed more than a third off its first: ` (처음 1,799 → 끝 105)`, in megabytes a second.
fn change(landing: &Landing) -> String {
    let rate = |step: &Step| step.bytes as f64 / step.seconds.max(1e-6);
    match &landing.steps[..] {
        [first, .., last] if (rate(first) - rate(last)).abs() > rate(first) / 3.0 => {
            format!(" (처음 {} → 끝 {})", megabytes_a_second(rate(first)), megabytes_a_second(rate(last)))
        }
        _ => String::new(),
    }
}

/// Why a write ended before all of it was written. `None` for one that wrote all of it.
fn ended(landing: &Landing, scrub: &Scrub) -> Option<String> {
    Some(match (landing.error, landing.halted) {
        (Some(0), _) => "쓰기 실패".to_owned(),
        (Some(code), _) => format!("쓰기 실패 (오류 {code})"),
        (None, Some(Halt::Drive)) => "여유 공간 부족".to_owned(),
        // The volume that went down with the writing is named.
        (None, Some(Halt::Elsewhere)) => {
            let half = landing.written / 2;
            let lost = |volume: &&crate::facts::Moved| matches!((volume.before, volume.after_write), (Some(before), Some(after)) if before.saturating_sub(after) >= half);
            let taken = landing.volumes.iter().filter(|volume| !volume.written_to).find(lost);
            on(&taken.map_or("다른 드라이브".to_owned(), |volume| scrub.text(&volume.letter)), "여유 부족")
        }
        (None, Some(Halt::Memory)) => "메모리 부족".to_owned(),
        (None, Some(Halt::Slow)) => "속도 급감".to_owned(),
        (None, Some(Halt::TimedOut)) => "시간 초과".to_owned(),
        (None, None) if landing.written < landing.wanted => "사용자 중단".to_owned(),
        (None, None) => return None,
    })
}

/// The volumes other than the one written to that had less free while a write went on,
/// each with the most it was down by: `쓰기 중 C: 여유 4 MB 감소`.
fn drained(landing: &Landing, scrub: &Scrub) -> Vec<String> {
    let mut said = Vec::new();
    for (at, volume) in landing.volumes.iter().enumerate().filter(|(_, volume)| !volume.written_to) {
        let Some(before) = volume.before else { continue };
        let during = landing.steps.iter().filter_map(|step| step.free.get(at).copied().flatten());
        let most = during.chain(volume.after_write).map(|now| before.saturating_sub(now)).max().unwrap_or(0);
        if most >= HALF_A_MEGABYTE {
            let lead = if said.is_empty() { "쓰기 중 " } else { "" };
            said.push(format!("{lead}{} 여유 {} 감소", scrub.text(&volume.letter), size(most)));
        }
    }
    said
}

/// What a write onto a volume found, as a row: how much was written and how fast, why it
/// ended early where it did, and what else went down with it. The write limit test
/// (`limit`) names the volume in what it says: its row is not named after one.
fn write_row(name: &str, landing: &Landing, limit: bool, scrub: &Scrub) -> Row {
    let volume = scrub.text(landing.written_to().unwrap_or_default());
    let place = |said: &str| if limit { on(&volume, said) } else { said.to_owned() };
    let why = ended(landing, scrub);
    let mut row = if landing.written == 0 {
        Row::with(name, &[place("쓰지 않음"), why.unwrap_or_default()])
    } else {
        let rate = landing.written as f64 / landing.seconds.max(1e-6);
        let row = Row::with(name, &[place(&size(landing.written)), format!("{}{}", speed(rate), change(landing))]);
        match why {
            Some(why) => row.under(&[format!("{}에서 중단", size(landing.written)), why]),
            None => row,
        }
    };
    row.lines.extend(lay(&drained(landing, scrub)));
    row
}

/// Whether the folder takes a patch, as far as it was tried: the empty file, and where
/// they were run, the write check on every volume and the write limit test.
fn writes(facts: &Facts, scrub: &Scrub) -> Vec<Row> {
    let failed = |code: Option<i32>| code.map_or("실패".to_owned(), |code| format!("실패 (오류 {code})"));
    let wrote = match facts.write_test {
        WriteTest::Passed | WriteTest::LeftBehind(_) => "성공".to_owned(),
        WriteTest::Failed(code) => failed(code),
        WriteTest::Skipped => "미실행".to_owned(),
    };
    let mut rows = vec![Row::of("설치 폴더 쓰기", wrote)];
    match facts.write_test {
        WriteTest::Passed => rows.push(Row::of("임시 파일 삭제", "성공")),
        WriteTest::LeftBehind(code) => rows.push(Row::of("임시 파일 삭제", failed(code))),
        WriteTest::Failed(_) | WriteTest::Skipped => {}
    }
    // What the check wrote and could not take away again, and where it is.
    let mut left: Vec<String> = Vec::new();
    for wrote in &facts.wrote {
        let found = match wrote {
            Wrote::EmptyFile { removed: false }
            | Wrote::ProbeFile { removed: false, .. }
            | Wrote::LimitFiles { removed: false, .. } => ".mfp-check-*.tmp (설치 폴더)".to_owned(),
            Wrote::OwnFolder { volume, removed: false, .. } => format!("{OWN_FOLDER} ({})", scrub.text(volume)),
            Wrote::Leftovers { left, .. } if *left > 0 => format!("이전 검사의 임시 파일 {left}개"),
            _ => continue,
        };
        if !left.contains(&found) {
            left.push(found);
        }
    }
    rows.push(if left.is_empty() { Row::of("잔여 파일", "없음") } else { Row::with("잔여 파일", &left) });

    let mut written: Vec<&Landing> = Vec::new();
    if let Asked::Done(landing) = &facts.landing {
        let volume = scrub.text(landing.written_to().unwrap_or_default());
        rows.push(write_row(&on(&volume, "쓰기 검사"), landing, false, scrub));
        written.push(landing);
    }
    for probed in &facts.other_landings {
        let name = on(&scrub.text(&probed.volume), "쓰기 검사");
        match &probed.landing {
            Ok(landing) => {
                rows.push(write_row(&name, landing, false, scrub));
                written.push(landing);
            }
            Err(code) => rows.push(Row::of(&name, failed(*code))),
        }
    }
    if let WriteLimit::Done(landing) = &facts.limit {
        rows.push(write_row("한도 검사", landing, true, scrub));
        written.push(landing);
    }
    if let Some(why) = limit_not_run(&facts.limit, scrub) {
        rows.push(Row::of("한도 검사", format!("미실행 ({why})")));
    }
    if !written.is_empty() {
        // One figure for all of it: the most memory was down while any of them wrote.
        let memory = match written.iter().filter_map(|landing| dip(landing)).max() {
            None => Unknown::NoAnswer.label(),
            Some(0..HALF_A_MEGABYTE) => "쓰기 중 감소 없음".to_owned(),
            Some(most) => format!("쓰기 중 최대 {} 감소", size(most)),
        };
        rows.push(Row::of("메모리 변화", memory));
    }
    rows
}

/// What the verdict rests on: the facts it was decided by, and no answer a second time.
fn analysis(judged: &Verdict) -> Vec<Row> {
    let rows = [
        ("PC 유형 근거", &judged.kind_grounds),
        ("읽기 속도 분석", &judged.speed_grounds),
        ("패치 판정 근거", &judged.patch_grounds),
        ("유지 판정 근거", &judged.kept_grounds),
    ];
    let said = rows.into_iter().filter(|(_, facts)| !facts.is_empty());
    said.map(|(name, facts)| Row { name: name.to_owned(), lines: grounds(facts) }).collect()
}

/// What the check itself did: how long it took, what it wrote where beyond the empty file,
/// and that nothing of the PC was sent anywhere.
fn record(facts: &Facts, scrub: &Scrub) -> Vec<Row> {
    let (base, all) = facts.took;
    let time = if all - base >= 0.5 { format!("{} (기본 검사 {})", took(all), took(base)) } else { took(base) };
    let mut rows = vec![Row::of("검사 소요 시간", time)];
    // Every place files were written to, with their sizes and whether they are gone again.
    let mut places: Vec<(String, &str, Vec<u64>, bool)> = Vec::new();
    let mut note = |name: String, what: &'static str, bytes: u64, removed: bool| match places
        .iter_mut()
        .find(|(known, ..)| *known == name)
    {
        Some((_, _, sizes, gone)) => {
            sizes.push(bytes);
            *gone &= removed;
        }
        None => places.push((name, what, vec![bytes], removed)),
    };
    let games = facts.game_drive.as_ref().map(|volume| scrub.text(&volume.letter)).unwrap_or_default();
    for wrote in &facts.wrote {
        match wrote {
            Wrote::ProbeFile { bytes, removed } | Wrote::LimitFiles { bytes, removed } => {
                note(on(&games, "설치 폴더"), "임시 파일", *bytes, *removed);
            }
            Wrote::OwnFolder { volume, place, bytes, removed, .. } => {
                let folder = match place {
                    Place::Temp => "시스템 임시 폴더의 mfp-check-tmp에",
                    Place::Root | Place::Install => "mfp-check-tmp에",
                };
                note(on(&scrub.text(volume), "임시 폴더"), folder, *bytes, *removed);
            }
            Wrote::EmptyFile { .. } | Wrote::Leftovers { .. } | Wrote::Report { .. } => {}
        }
    }
    for (name, what, sizes, removed) in places {
        let sizes: Vec<String> = sizes.into_iter().filter(|bytes| *bytes > 0).map(size).collect();
        let made = if sizes.is_empty() { format!("{what} 폴더만") } else { format!("{what} {}", sizes.join(" / ")) };
        rows.push(if removed {
            Row::of(&name, format!("{made} 생성 후 삭제"))
        } else {
            Row::with(&name, &[format!("{made} 생성"), "삭제 실패".to_owned()])
        });
    }
    for wrote in &facts.wrote {
        match wrote {
            Wrote::Leftovers { removed, left } => {
                let gone = if *removed > 0 { format!("{removed}개 삭제") } else { String::new() };
                let stayed = if *left > 0 { format!("{left}개 삭제 실패") } else { String::new() };
                rows.push(Row::with("이전 검사 파일", &[gone, stayed]));
            }
            Wrote::Report { file_name } => {
                rows.push(Row::of("검사 결과 저장", format!("실행 파일 폴더의 {file_name}")))
            }
            _ => {}
        }
    }
    rows.push(Row::of("PC 정보 전송", "없음"));
    rows
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use mfp_kms::Server;

    use super::*;
    use crate::compare::Tally;
    use crate::facts::{
        Adapter, Bus, Comparison, Game, How, Memory, Moved, Probed, Programs, Reads, Registry, Rights, Room, Shelf,
        Traffic, WebView,
    };

    const GIB: u64 = 1 << 30;
    const MIB: u64 = 1 << 20;
    /// A megabyte a second, as speeds are counted here.
    const MB_S: f64 = MIB as f64;
    /// Words that hedge. A report states what was found and holds none of them.
    const HEDGES: [&str; 9] = ["예상", "판단 보류", "보임", "것 같", "수 있", "추정", "아마", "듯", "가능성"];

    /// A tenth of a gigabyte, for figures as a report shows them: `305.2 GB` is 3052 of these.
    fn tenths_of_gb(tenths: u64) -> u64 {
        tenths * GIB / 10 + MIB
    }

    fn nobody() -> Scrub {
        Scrub::new(Vec::new(), Vec::new())
    }

    /// The report of `facts`, held to what every report is held to: no line wider than a
    /// report is, and no word that hedges.
    fn checked(facts: &Facts, scrub: &Scrub, head: &Head) -> String {
        let report = render(facts, scrub, head);
        for line in report.lines() {
            assert!(width(line) <= WIDTH, "{} columns in {line:?}", width(line));
        }
        for hedge in HEDGES {
            assert!(!report.contains(hedge), "{hedge:?} hedges in\n{report}");
        }
        report
    }

    fn named(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    /// A volume by the tenths of a gigabyte it has in all and free, on the disk Windows
    /// numbers `disk`.
    fn volume(letter: &str, bus: Bus, model: &str, system: &str, room: (u64, u64), disk: u32) -> Volume {
        Volume {
            letter: letter.to_owned(),
            kind: DriveKind::Fixed,
            file_system: Ok(system.to_owned()),
            label: String::new(),
            read_only: false,
            bus: Ok(bus),
            model: model.to_owned(),
            seek_penalty: Ok(false),
            total: Ok(tenths_of_gb(room.0)),
            free: Ok(tenths_of_gb(room.1)),
            disk: Ok(disk),
        }
    }

    const NEWEST: State = State { version: 421, minor: 4 };

    /// The comparison that reads nothing, of a folder that is at the newest version.
    fn by_size(not_in_table: u64) -> Comparison {
        Comparison {
            by_length: true,
            at: Some(NEWEST),
            either: Vec::new(),
            against: Some((NEWEST, Source::Table)),
            tally: Tally { same_length: 717, unread: 495, ..Tally::default() },
            not_in_table,
            bytes_read: 0,
            seconds: 0.0,
            cut_short: false,
        }
    }

    /// A comparison that read files: `bytes` of them in `seconds`.
    fn by_content(tally: Tally, bytes: u64, seconds: f64) -> Comparison {
        Comparison { by_length: false, tally, bytes_read: bytes, seconds, ..by_size(13) }
    }

    fn shelf(megabytes_a_second: f64, round_trip: f64) -> Shelf {
        Shelf {
            newest: NEWEST,
            patch: None,
            patch_is_next_main: false,
            sample: Ok((8 << 20, 8.0 / megabytes_a_second)),
            round_trip: Ok(round_trip),
        }
    }

    fn wired() -> Fact<Vec<Adapter>> {
        Ok(vec![Adapter {
            model: "Realtek Gaming GbE Family Controller".to_owned(),
            wireless: false,
            speed: Some(1_000_000_000),
        }])
    }

    /// The drivers for server adapters a Windows that is made to start on many PCs loads.
    const SPARE: [&str; 11] = [
        "3ware",
        "ADP80XX",
        "arcsas",
        "cht4iscsi",
        "megasas",
        "megasas2i",
        "megasas35i",
        "megasr",
        "percsas2i",
        "percsas3i",
        "stexstor",
    ];

    /// A PC room seat whose disks call themselves NVMe disks and are its server's, after
    /// the check that asks nothing: 2026.11.27 01:19.
    fn seat_by_default() -> Facts {
        let disk = |letter: &str, system: &str, room: (u64, u64), number: u32| {
            volume(letter, Bus::NVME, "NetZone NxDDisk", system, room, number)
        };
        Facts {
            started_from: Ok(DriveKind::Fixed),
            windows: "Windows 10 Pro (19045) · 64비트".to_owned(),
            cpu: "13th Gen Intel(R) Core(TM) i5-13400F".to_owned(),
            cores: 10,
            threads: 16,
            ram: tenths_of_gb(158),
            rights: Ok(Rights { administrator: true, elevated: true, asks_first: Some(false) }),
            webview: Ok(Some(WebView { version: "150.0.4078.48".to_owned(), for_all: true })),
            adapters: wired(),
            system_drive: Ok(disk("C:", "NTFS", (800, 430), 0)),
            how: How::Registry,
            install: Some(PathBuf::from(r"D:\Games\Maple")),
            registry: Registry { install_dir: Some(r"D:\Games\Maple".to_owned()), version_hint: Some(421) },
            game_drive: Ok(disk("D:", "NTFS", (74518, 25633), 1)),
            other_volumes: vec![disk("S:", "exFAT", (200, 40), 2)],
            game: Some(Game {
                file_version: Ok([1, 421, 4, 0]),
                patcher_log: Some("00421_04".to_owned()),
                files: Ok((1234, tenths_of_gb(634))),
                app_traces: false,
            }),
            reads: Ok(Reads {
                bytes: 216 << 20,
                first: 108.0 * MB_S,
                again: 108.0 * MB_S,
                fresh: None,
                seek_ms: Some(0.2),
            }),
            shelf: Ok(shelf(15.0, 12.4)),
            took: (5.2, 5.2),
            sizes: Ok(by_size(22)),
            full: Asked::Declined,
            write_test: WriteTest::Passed,
            landing: Asked::Declined,
            limit: WriteLimit::Declined,
            programs: Ok(Programs {
                seat: named(&["GreenGuard.exe"]),
                iscsi: named(&["iScsiPrt"]),
                spare: named(&SPARE),
                noted: named(&["ahcache", "ShaderCacheClient.exe"]),
                game: named(&["MapleStory.exe"]),
                ..Programs::default()
            }),
            drivers_listed: Ok(()),
            wrote: vec![Wrote::EmptyFile { removed: true }],
            traffic: Traffic { requests: 8, received: 8 << 20 },
            ..Facts::none_yet(1_795_709_940)
        }
    }

    /// What Windows says of its memory when this much is available.
    fn memory(available: u64) -> Option<Memory> {
        Some(Memory { available, cache: 6 * GIB, nonpaged: 400 * MIB, commit: 9 * GIB })
    }

    /// A write onto `written`, one of the volumes `began` names with what each had free:
    /// steps of `step` bytes at these megabytes a second, after each of which this many
    /// megabytes less memory were available than the 14.1 GB there were before.
    fn wrote_at(
        written: &str,
        place: Place,
        began: &[(&str, u64)],
        step: u64,
        speeds: &[f64],
        down: &[u64],
    ) -> Landing {
        let before = tenths_of_gb(141);
        let free_after = |bytes: u64| -> Vec<Option<u64>> {
            began.iter().map(|(letter, free)| Some(if *letter == written { free - bytes } else { *free })).collect()
        };
        let megabytes = step as f64 / MB_S;
        let steps: Vec<Step> = speeds
            .iter()
            .zip(down)
            .zip(1..)
            .map(|((speed, down), at)| Step {
                bytes: step,
                seconds: megabytes / speed,
                free: free_after(at * step),
                memory: memory(before - down * MIB),
            })
            .collect();
        let all = step * speeds.len() as u64;
        let volumes = began
            .iter()
            .zip(free_after(all))
            .map(|((letter, free), after_write)| Moved {
                letter: (*letter).to_owned(),
                written_to: *letter == written,
                before: Some(*free),
                after_write,
                after_delete: Some(*free),
                later: Some(*free),
            })
            .collect();
        let after = memory(before - down.last().copied().unwrap_or(0) * MIB);
        Landing {
            place,
            written: all,
            wanted: all,
            seconds: steps.iter().map(|step| step.seconds).sum(),
            error: None,
            halted: None,
            steps,
            volumes,
            memory: [memory(before), after, after, memory(before - 101 * MIB)],
            waited: 3.0,
            deleted: true,
        }
    }

    /// The first PC room seat the check was run at, with every step that is asked for run:
    /// its disks come from its server by Fibre Channel, the write check went to all three
    /// volumes, and the write limit test ended itself at 4 GB. 2026.11.26 23:49.
    fn seat_with_every_step() -> Facts {
        let served = |letter: &str, model: &str, room: (u64, u64), number: u32| Volume {
            seek_penalty: Err(Unknown::Windows(1)),
            ..volume(letter, Bus::FIBRE_CHANNEL, model, "NTFS", room, number)
        };
        let began = [("C:", tenths_of_gb(1540)), ("D:", tenths_of_gb(328)), ("G:", tenths_of_gb(3052))];
        let probe = |written: &str, place: Place, speeds: [f64; 4], down: [u64; 4]| {
            wrote_at(written, place, &began, 256 * MIB, &speeds, &down)
        };
        let limit = Landing {
            wanted: ROOM_NEEDED,
            halted: Some(Halt::Slow),
            ..wrote_at("G:", Place::Install, &began, GIB, &[1291.0, 107.0, 106.0, 107.0], &[0, 12, 30, 37])
        };
        let own = |volume: &str| Wrote::OwnFolder {
            volume: volume.to_owned(),
            place: Place::Root,
            bytes: GIB,
            limit: false,
            removed: true,
        };
        Facts {
            cpu: "12th Gen Intel(R) Core(TM) i5-12400F".to_owned(),
            cores: 6,
            threads: 12,
            ram: tenths_of_gb(318),
            webview: Ok(Some(WebView { version: "158.0.4370.33".to_owned(), for_all: true })),
            system_drive: Ok(served("C:", "NSM 22H2-H610 232GB", (2000, 1540), 0)),
            install: Some(PathBuf::from(r"G:\Gamez\Maple")),
            registry: Registry { install_dir: Some(r"G:\Gamez\Maple".to_owned()), version_hint: Some(421) },
            game_drive: Ok(served("G:", "NSM Logical_G 3722GB", (37227, 3052), 1)),
            other_volumes: vec![served("D:", "NSM 22H2-H610 232GB", (329, 328), 0)],
            game: Some(Game {
                file_version: Ok([1, 421, 4, 0]),
                patcher_log: Some("00421_04".to_owned()),
                files: Ok((1225, tenths_of_gb(634))),
                app_traces: false,
            }),
            reads: Ok(Reads {
                bytes: 212 << 20,
                first: 106.0 * MB_S,
                again: 104.0 * MB_S,
                fresh: None,
                seek_ms: Some(0.7),
            }),
            shelf: Ok(shelf(37.0, 4.66)),
            took: (4.7, 771.0),
            sizes: Ok(by_size(13)),
            quick: Ok(by_content(
                Tally { same: 750, same_length: 222, unread: 240, ..Tally::default() },
                tenths_of_gb(58),
                45.0,
            )),
            full: Asked::Done(by_content(Tally { same: 1212, ..Tally::default() }, tenths_of_gb(633), 586.0)),
            landing: Asked::Done(probe("G:", Place::Install, [1799.0, 1799.0, 106.0, 105.0], [3, 47, 162, 154])),
            other_landings: vec![
                Probed {
                    volume: "C:".to_owned(),
                    landing: Ok(probe("C:", Place::Root, [124.0, 114.0, 110.0, 109.0], [54, 53, 78, 85])),
                },
                Probed {
                    volume: "D:".to_owned(),
                    landing: Ok(probe("D:", Place::Root, [121.0, 110.0, 108.0, 111.0], [89, 123, 183, 150])),
                },
            ],
            limit: WriteLimit::Done(limit),
            programs: Ok(Programs {
                seat: named(&["PicaigreenClient.exe", "PicaVncSvc.exe", "pmclient.exe"]),
                iscsi: named(&["iScsiPrt"]),
                spare: named(&SPARE),
                noted: named(&["ahcache", "NVHDA"]),
                game: named(&["MapleStory.exe"]),
                ..Programs::default()
            }),
            wrote: vec![
                Wrote::EmptyFile { removed: true },
                Wrote::ProbeFile { bytes: GIB, removed: true },
                own("C:"),
                own("D:"),
                Wrote::LimitFiles { bytes: 4 * GIB, removed: true },
            ],
            when: 1_795_704_540,
            ..seat_by_default()
        }
    }

    /// A PC at home after the check that asks nothing: Windows on one NVMe disk, the game
    /// on another, on Wi-Fi, the program run without an administrator's rights. As a
    /// caller has it checked that says what the server has and that the folder takes
    /// writes: no server asked, no empty file made. 2026.11.27 01:36.
    fn home() -> Facts {
        Facts {
            windows: "Windows 11 Pro (26200) · 64비트".to_owned(),
            cpu: "11th Gen Intel(R) Core(TM) i7-11800H @ 2.30GHz".to_owned(),
            cores: 8,
            threads: 16,
            ram: tenths_of_gb(158),
            rights: Ok(Rights { administrator: true, elevated: false, asks_first: Some(true) }),
            webview: Ok(Some(WebView { version: "158.0.4370.33".to_owned(), for_all: true })),
            adapters: Ok(vec![Adapter {
                model: "Intel(R) Wi-Fi 6 AX201 160MHz".to_owned(),
                wireless: true,
                speed: Some(2_400_000_000),
            }]),
            system_drive: Ok(volume("C:", Bus::NVME, "Micron MTFDKBA512TFH", "NTFS", (4756, 794), 0)),
            game_drive: Ok(volume("D:", Bus::NVME, "Samsung SSD 980 1TB", "NTFS", (9315, 2000), 1)),
            other_volumes: Vec::new(),
            game: Some(Game {
                file_version: Ok([1, 421, 4, 0]),
                patcher_log: Some("00421_04".to_owned()),
                files: Ok((1225, tenths_of_gb(634))),
                app_traces: false,
            }),
            reads: Ok(Reads {
                bytes: 256 << 20,
                first: 2150.0 * MB_S,
                again: 2230.0 * MB_S,
                fresh: None,
                seek_ms: Some(0.1),
            }),
            shelf: Ok(Shelf { sample: Err(Unknown::Skipped), round_trip: Err(Unknown::Skipped), ..shelf(1.0, 1.0) }),
            took: (0.4, 0.4),
            sizes: Ok(by_size(13)),
            programs: Ok(Programs { noted: named(&["ahcache", "NVHDA"]), ..Programs::default() }),
            wrote: Vec::new(),
            traffic: Traffic::default(),
            when: 1_795_710_960,
            ..seat_by_default()
        }
    }

    /// The same PC as the app has it checked: told what the server has, the check times
    /// the line to it all the same, which is the one thing it asks of the server.
    fn home_with_the_line_timed() -> Facts {
        Facts { shelf: Ok(shelf(42.0, 8.3)), traffic: Traffic { requests: 2, received: 8 << 20 }, ..home() }
    }

    /// The same PC with three of the game's files gone.
    fn home_with_files_missing() -> Facts {
        let gone =
            named(&[r"Data\Map\Map001\Map001_000.wz", r"Data\Map\Map001\Map001_001.wz", r"Data\Sound\Sound_012.wz"]);
        let sizes = Comparison {
            at: None,
            tally: Tally { same_length: 714, unread: 495, missing: gone, ..Tally::default() },
            ..by_size(13)
        };
        let files = Ok((1222, tenths_of_gb(629)));
        Facts { sizes: Ok(sizes), game: Some(Game { files, ..home().game.unwrap() }), ..home() }
    }

    /// A PC on which Windows would tell next to nothing, and no install folder.
    fn locked_down() -> Facts {
        Facts {
            windows: "확인 실패 · 64비트".to_owned(),
            cores: 4,
            threads: 4,
            started_from: Err(Unknown::Windows(5)),
            rights: Err(Unknown::Windows(5)),
            webview: Err(Unknown::Windows(5)),
            adapters: Err(Unknown::Windows(1168)),
            system_drive: Err(Unknown::Windows(21)),
            programs: Err(Unknown::Windows(5)),
            drivers_listed: Err(Unknown::Windows(5)),
            shelf: Err(Unknown::NoConnection),
            took: (0.3, 0.3),
            ..Facts::none_yet(1_795_710_960)
        }
    }

    /// The reports that are handed over as samples, by the names of their files.
    fn samples() -> [(&'static str, String); 6] {
        let app = Head::App("0.1.0".to_owned());
        [
            ("report-seat-full.txt", checked(&seat_with_every_step(), &nobody(), &Head::Check)),
            ("report-seat-default.txt", checked(&seat_by_default(), &nobody(), &app)),
            ("report-home.txt", checked(&home(), &nobody(), &app)),
            ("report-app-home.txt", checked(&home_with_the_line_timed(), &nobody(), &app)),
            ("report-missing.txt", checked(&home_with_files_missing(), &nobody(), &app)),
            ("report-unknown.txt", checked(&locked_down(), &nobody(), &Head::Check)),
        ]
    }

    /// The rows of a group of a report, each as its name and what it says, line by line.
    fn rows(report: &str, title: &str) -> Vec<(String, Vec<String>)> {
        let heading = format!("[{title}]");
        let lines = report.lines().skip_while(|line| *line != heading).skip(1).take_while(|line| !line.is_empty());
        let mut rows: Vec<(String, Vec<String>)> = Vec::new();
        for line in lines {
            let (mut at, mut split) = (0, line.len());
            for (place, letter) in line.char_indices() {
                if at >= VALUES_AT {
                    split = place;
                    break;
                }
                at += columns(letter);
            }
            let (name, said) = (line[..split].trim_end(), line[split..].to_owned());
            match rows.last_mut() {
                Some((_, lines)) if name.is_empty() => lines.push(said),
                _ => rows.push((name.to_owned(), vec![said])),
            }
        }
        rows
    }

    /// What the row `name` of a group says, its lines joined by `|`.
    fn said(report: &str, title: &str, name: &str) -> Option<String> {
        rows(report, title).into_iter().find(|(row, _)| row == name).map(|(_, lines)| lines.join("|"))
    }

    /// Writes the sample reports to the folder `MFP_REPORTS_DIR` names, for whoever wants
    /// to read them: `cargo test -p mfp-check --lib sample_reports -- --ignored`.
    #[test]
    #[ignore = "writes files"]
    fn sample_reports_are_written_for_whoever_asks() {
        let folder = PathBuf::from(std::env::var_os("MFP_REPORTS_DIR").expect("MFP_REPORTS_DIR names no folder"));
        std::fs::create_dir_all(&folder).unwrap();
        for (name, report) in samples() {
            std::fs::write(folder.join(name), report).unwrap();
        }
    }

    /// One of the sample reports, by the name of its file.
    fn sample(name: &str) -> String {
        samples().into_iter().find(|(file, _)| *file == name).map(|(_, report)| report).expect(name)
    }

    /// A seat without a disk after the check that asks nothing, as the app shows it: the
    /// text the format was laid down by, to the letter.
    #[test]
    fn report_of_a_seat_after_the_check_that_asks_nothing_is_this_text() {
        let expected = r"Maple Fast Patch 0.1.0 · 2026.11.27 01:19

[검사 요약]
전체 검사 결과    주의
패치 가능 여부    가능
패치 공간         충분 (D: 여유 2,563.3 GB · 필요 36.5 GB)
패치 유지 여부    불가능 (재부팅·로그아웃 시 초기화)
자리 이동         재패치 필요

[PC 환경]
사용 장소         PC방
디스크 유형       노하드 (서버 디스크 방식)
게임 저장 위치    매장 서버 (네트워크)
운영체제          Windows 10 Pro (19045) · 64비트
CPU               Intel Core i5-13400F · 10코어 16스레드
메모리            15.8 GB
실행 권한         관리자 · UAC 비활성화
WebView2          150.0.4078.48

[디스크 환경]
C: 시스템         NetZone NxDDisk · NVMe · NTFS
                  여유 43.0 / 80.0 GB
D: 게임           NetZone NxDDisk · NVMe · NTFS
                  여유 2,563.3 / 7,451.8 GB
S: 기타           NetZone NxDDisk · NVMe · exFAT
                  여유 4.0 / 20.0 GB
디스크 구성       C:, D:, S: 각각 별도 디스크

[게임 설치 상태]
게임 이름         메이플스토리
설치 경로         D:\Games\Maple
현재 게임 버전    Ver 1.2.421 마이너버전(4)
최신 게임 버전    Ver 1.2.421 마이너버전(4)
설치 용량         63.4 GB
전체 파일 개수    1,234개

[게임 파일 검사]
검사 기준         파일 크기 비교
검사 대상 버전    Ver 1.2.421 마이너버전(4)
정상 파일         1,212개
변경 파일         없음
누락 파일         없음
검사 제외 파일    22개

[네트워크 환경]
연결 방식         유선 LAN
네트워크 속도     1 Gbps
다운로드 속도     15 MB/s
응답 지연 시간    12 ms

[성능 측정]
디스크 읽기 속도  108 MB/s
반복 읽기 속도    108 MB/s

[패치 환경 검사]
설치 폴더 쓰기    성공
임시 파일 삭제    성공
잔여 파일         없음

[환경 분석]
PC 유형 근거      NetZone 디스크 · GreenGuard.exe · iScsiPrt
읽기 속도 분석    네트워크 전송 속도와 유사한 읽기 패턴
유지 판정 근거    노하드 좌석은 재부팅·로그아웃 시 변경 내용 초기화

[검사 기록]
검사 소요 시간    5.2초
PC 정보 전송      없음";
        assert_eq!(sample("report-seat-default.txt"), expected);
    }

    /// The first seat the check was run at, with every step that is asked for run, as the
    /// console prints it: the write check on its three volumes, the write limit test that
    /// ended itself, and both comparisons that read.
    #[test]
    fn report_of_a_seat_with_every_step_run_is_this_text() {
        let rest = r"
[검사 요약]
전체 검사 결과    주의
패치 가능 여부    가능
패치 공간         충분 (G: 여유 305.2 GB · 필요 36.5 GB)
패치 유지 여부    불가능 (재부팅·로그아웃 시 초기화)
자리 이동         재패치 필요

[PC 환경]
사용 장소         PC방
디스크 유형       노하드 (서버 디스크 방식)
게임 저장 위치    매장 서버 (네트워크)
운영체제          Windows 10 Pro (19045) · 64비트
CPU               Intel Core i5-12400F · 6코어 12스레드
메모리            31.8 GB
실행 권한         관리자 · UAC 비활성화
WebView2          158.0.4370.33

[디스크 환경]
C: 시스템         NSM 22H2-H610 232GB · Fibre Channel · NTFS
                  여유 154.0 / 200.0 GB
G: 게임           NSM Logical_G 3722GB · Fibre Channel · NTFS
                  여유 305.2 / 3,722.7 GB
D: 기타           NSM 22H2-H610 232GB · Fibre Channel · NTFS
                  여유 32.8 / 32.9 GB
디스크 구성       C:, D: 같은 디스크 · G: 별도 디스크

[게임 설치 상태]
게임 이름         메이플스토리
설치 경로         G:\Gamez\Maple
현재 게임 버전    Ver 1.2.421 마이너버전(4)
최신 게임 버전    Ver 1.2.421 마이너버전(4)
설치 용량         63.4 GB
전체 파일 개수    1,225개

[게임 파일 검사]
검사 기준         파일 내용 비교 (전체)
검사 대상 버전    Ver 1.2.421 마이너버전(4)
정상 파일         1,212개
변경 파일         없음
누락 파일         없음
검사 제외 파일    13개
검사 시간         9분 46초 · 63.3 GB

[네트워크 환경]
연결 방식         유선 LAN
네트워크 속도     1 Gbps
다운로드 속도     37 MB/s
응답 지연 시간    4.7 ms

[성능 측정]
디스크 읽기 속도  106 MB/s
반복 읽기 속도    104 MB/s

[패치 환경 검사]
설치 폴더 쓰기    성공
임시 파일 삭제    성공
잔여 파일         없음
G: 쓰기 검사      1.0 GB · 199 MB/s (처음 1,799 → 끝 105)
C: 쓰기 검사      1.0 GB · 114 MB/s
D: 쓰기 검사      1.0 GB · 112 MB/s
한도 검사         G: 4.0 GB · 138 MB/s (처음 1,291 → 끝 107)
                  4.0 GB에서 중단 · 속도 급감
메모리 변화       쓰기 중 최대 183 MB 감소

[환경 분석]
PC 유형 근거      Fibre Channel 디스크 · NSM 디스크 · PicaigreenClient.exe,
                  PicaVncSvc.exe, pmclient.exe · iScsiPrt
읽기 속도 분석    네트워크 전송 속도와 유사한 읽기 패턴
패치 판정 근거    쓰기 후반 속도가 네트워크 전송 속도와 유사
                  쓰기 중 메모리 162 MB 감소 · 한도 검사 4.0 GB에서 중단
유지 판정 근거    노하드 좌석은 재부팅·로그아웃 시 변경 내용 초기화

[검사 기록]
검사 소요 시간    12분 51초 (기본 검사 4.7초)
G: 설치 폴더      임시 파일 1.0 GB / 4.0 GB 생성 후 삭제
C: 임시 폴더      mfp-check-tmp에 1.0 GB 생성 후 삭제
D: 임시 폴더      mfp-check-tmp에 1.0 GB 생성 후 삭제
PC 정보 전송      없음";
        let first = format!("Maple Fast Patch 검사 {} · 2026.11.26 23:49", env!("CARGO_PKG_VERSION"));
        assert_eq!(sample("report-seat-full.txt"), format!("{first}\n{rest}"));
    }

    /// A PC at home after the check that asks nothing, as the app shows it.
    #[test]
    fn report_of_an_ordinary_pc_after_the_check_that_asks_nothing_is_this_text() {
        let expected = r"Maple Fast Patch 0.1.0 · 2026.11.27 01:36

[검사 요약]
전체 검사 결과    정상
패치 가능 여부    가능
패치 공간         충분 (D: 여유 200.0 GB · 필요 36.5 GB)
패치 유지 여부    가능

[PC 환경]
사용 장소         일반 PC
디스크 유형       PC 내장 디스크
게임 저장 위치    이 PC의 디스크
운영체제          Windows 11 Pro (26200) · 64비트
CPU               Intel Core i7-11800H · 8코어 16스레드
메모리            15.8 GB
실행 권한         일반 사용자 · UAC 활성화
WebView2          158.0.4370.33

[디스크 환경]
C: 시스템         Micron MTFDKBA512TFH · NVMe · NTFS
                  여유 79.4 / 475.6 GB
D: 게임           Samsung SSD 980 1TB · NVMe · NTFS
                  여유 200.0 / 931.5 GB
디스크 구성       C:, D: 각각 별도 디스크

[게임 설치 상태]
게임 이름         메이플스토리
설치 경로         D:\Games\Maple
현재 게임 버전    Ver 1.2.421 마이너버전(4)
최신 게임 버전    Ver 1.2.421 마이너버전(4)
설치 용량         63.4 GB
전체 파일 개수    1,225개

[게임 파일 검사]
검사 기준         파일 크기 비교
검사 대상 버전    Ver 1.2.421 마이너버전(4)
정상 파일         1,212개
변경 파일         없음
누락 파일         없음
검사 제외 파일    13개

[네트워크 환경]
연결 방식         무선 Wi-Fi
네트워크 속도     2.4 Gbps
다운로드 속도     미측정
응답 지연 시간    미측정

[성능 측정]
디스크 읽기 속도  2,150 MB/s
반복 읽기 속도    2,230 MB/s

[패치 환경 검사]
설치 폴더 쓰기    성공
임시 파일 삭제    성공
잔여 파일         없음

[환경 분석]
PC 유형 근거      이 PC의 NVMe 디스크에 설치
패치 판정 근거    D: 여유 200.0 GB · 설치 폴더 쓰기 성공
유지 판정 근거    노하드·복구 프로그램 없음

[검사 기록]
검사 소요 시간    0.4초
PC 정보 전송      없음";
        assert_eq!(sample("report-home.txt"), expected);
    }

    /// The app says what the server has and has the line timed: the two rows of the line
    /// carry what was measured, and nothing else of the report is another.
    #[test]
    fn report_of_the_apps_check_says_how_fast_the_line_to_the_server_is() {
        let report = sample("report-app-home.txt");
        let network: Vec<String> = rows(&report, "네트워크 환경")
            .into_iter()
            .map(|(name, said)| format!("{name}: {}", said.join("|")))
            .collect();
        assert_eq!(
            network,
            ["연결 방식: 무선 Wi-Fi", "네트워크 속도: 2.4 Gbps", "다운로드 속도: 42 MB/s", "응답 지연 시간: 8.3 ms"]
        );
        let untimed = sample("report-home.txt");
        let other: Vec<(&str, &str)> =
            report.lines().zip(untimed.lines()).filter(|(timed, not)| timed != not).collect();
        assert_eq!(
            other,
            [
                ("다운로드 속도     42 MB/s", "다운로드 속도     미측정"),
                ("응답 지연 시간    8.3 ms", "응답 지연 시간    미측정")
            ]
        );
        assert_eq!(report.lines().count(), untimed.lines().count());
    }

    /// The same PC with three of the game's files gone: the patch is said not to go
    /// through, and the files are named.
    #[test]
    fn report_of_a_pc_with_files_missing_is_this_text() {
        let expected = r"Maple Fast Patch 0.1.0 · 2026.11.27 01:36

[검사 요약]
전체 검사 결과    패치 불가
패치 가능 여부    불가능 (게임 파일 없음(3개))
패치 공간         충분 (D: 여유 200.0 GB · 필요 36.5 GB)
패치 유지 여부    가능

[PC 환경]
사용 장소         일반 PC
디스크 유형       PC 내장 디스크
게임 저장 위치    이 PC의 디스크
운영체제          Windows 11 Pro (26200) · 64비트
CPU               Intel Core i7-11800H · 8코어 16스레드
메모리            15.8 GB
실행 권한         일반 사용자 · UAC 활성화
WebView2          158.0.4370.33

[디스크 환경]
C: 시스템         Micron MTFDKBA512TFH · NVMe · NTFS
                  여유 79.4 / 475.6 GB
D: 게임           Samsung SSD 980 1TB · NVMe · NTFS
                  여유 200.0 / 931.5 GB
디스크 구성       C:, D: 각각 별도 디스크

[게임 설치 상태]
게임 이름         메이플스토리
설치 경로         D:\Games\Maple
현재 게임 버전    Ver 1.2.421 마이너버전(4)
최신 게임 버전    Ver 1.2.421 마이너버전(4)
설치 용량         62.9 GB
전체 파일 개수    1,222개

[게임 파일 검사]
검사 기준         파일 크기 비교
검사 대상 버전    Ver 1.2.421 마이너버전(4) · 일치 버전 없음
정상 파일         1,209개
변경 파일         없음
누락 파일         3개
                  Data\Map\Map001\Map001_000.wz,
                  Data\Map\Map001\Map001_001.wz, Data\Sound\Sound_012.wz
검사 제외 파일    13개

[네트워크 환경]
연결 방식         무선 Wi-Fi
네트워크 속도     2.4 Gbps
다운로드 속도     미측정
응답 지연 시간    미측정

[성능 측정]
디스크 읽기 속도  2,150 MB/s
반복 읽기 속도    2,230 MB/s

[패치 환경 검사]
설치 폴더 쓰기    성공
임시 파일 삭제    성공
잔여 파일         없음

[환경 분석]
PC 유형 근거      이 PC의 NVMe 디스크에 설치
패치 판정 근거    게임 파일 3개 누락
유지 판정 근거    노하드·복구 프로그램 없음

[검사 기록]
검사 소요 시간    0.4초
PC 정보 전송      없음";
        assert_eq!(sample("report-missing.txt"), expected);
    }

    #[test]
    fn every_line_keeps_to_the_columns_and_nothing_hedges() {
        for (name, report) in samples() {
            let mut lines = report.lines();
            let mut before = lines.next().unwrap();
            assert!(before.starts_with("Maple Fast Patch "), "{name}");
            for line in lines {
                assert!(width(line) <= WIDTH, "{name}: {} columns in {line:?}", width(line));
                assert!(!line.ends_with(' ') && !line.contains('\t'), "{name}: {line:?}");
                if line.starts_with('[') {
                    // An empty line stands before every heading, and nowhere else.
                    assert!(before.is_empty() && line.ends_with(']'), "{name}: {line:?} after {before:?}");
                } else if !line.is_empty() {
                    // What is said begins at the one column, after two blanks or more.
                    let (mut at, mut lead) = (0, String::new());
                    for letter in line.chars() {
                        if at >= VALUES_AT {
                            assert_ne!(letter, ' ', "{name}: {line:?}");
                            break;
                        }
                        lead.push(letter);
                        at += columns(letter);
                    }
                    assert!(at == VALUES_AT && lead.ends_with("  "), "{name}: {line:?}");
                    assert!(width(lead.trim_end()) <= VALUES_AT - 2, "{name}: {line:?}");
                } else {
                    assert!(!before.is_empty(), "{name}: two empty lines");
                }
                before = line;
            }
            assert!(!before.is_empty(), "{name}: the report ends in an empty line");
            for hedge in HEDGES {
                assert!(!report.contains(hedge), "{name} hedges with {hedge:?}");
            }
        }
    }

    #[test]
    fn first_line_names_the_program_the_report_is_out_of() {
        let facts = seat_by_default();
        let own = checked(&facts, &nobody(), &Head::Check);
        assert_eq!(
            own.lines().next(),
            Some(format!("Maple Fast Patch 검사 {} · 2026.11.27 01:19", facts.tool_version).as_str())
        );
        let apps = checked(&facts, &nobody(), &Head::App("0.1.0".to_owned()));
        assert_eq!(apps.lines().next(), Some("Maple Fast Patch 0.1.0 · 2026.11.27 01:19"));
        // Nothing else of the report is another.
        assert_eq!(own.split_once('\n').unwrap().1, apps.split_once('\n').unwrap().1);
        assert_eq!(apps.matches("Maple Fast Patch").count(), 1);
    }

    #[test]
    fn summary_says_the_verdict_and_judges_nothing_itself() {
        let summary = |facts: &Facts| -> Vec<String> {
            let report = checked(facts, &nobody(), &Head::Check);
            rows(&report, "검사 요약").into_iter().map(|(name, lines)| format!("{name}: {}", lines.join("|"))).collect()
        };
        // A PC at home with the room: all is well.
        assert_eq!(
            summary(&home()),
            [
                "전체 검사 결과: 정상",
                "패치 가능 여부: 가능",
                "패치 공간: 충분 (D: 여유 200.0 GB · 필요 36.5 GB)",
                "패치 유지 여부: 가능",
            ]
        );
        // Too little room for the usual way. Whether the immediate way fits is in the
        // patch file: with the engine's figures it is said, and the room is theirs.
        let tight = |free: u64, room: Option<Room>| Facts {
            game_drive: Ok(volume("D:", Bus::NVME, "Samsung SSD 980 1TB", "NTFS", (9315, free), 1)),
            room,
            ..home()
        };
        let room = Some(Room { safe: 71 * GIB / 2, immediate: 9 * GIB });
        assert_eq!(
            summary(&tight(200, room))[..3],
            [
                "전체 검사 결과: 주의",
                "패치 가능 여부: 즉시 패치로 가능",
                "패치 공간: 부족 (D: 여유 20.0 GB · 필요 35.5 GB)"
            ]
        );
        assert_eq!(
            summary(&tight(50, room))[..3],
            [
                "전체 검사 결과: 패치 불가",
                "패치 가능 여부: 불가능 (여유 공간 부족(5 GB))",
                "패치 공간: 부족 (D: 여유 5.0 GB · 필요 35.5 GB)"
            ]
        );
        assert_eq!(
            summary(&tight(200, None))[..3],
            [
                "전체 검사 결과: 확인 불가",
                "패치 가능 여부: 확인 불가",
                "패치 공간: 부족 (D: 여유 20.0 GB · 필요 36.5 GB)"
            ]
        );
        // A disk that a program puts back: the patch goes through, and nothing says
        // whether it stays.
        let restored =
            Facts { programs: Ok(Programs { restore: named(&["DFServ.exe"]), ..Programs::default() }), ..home() };
        assert_eq!(summary(&restored)[0], "전체 검사 결과: 주의");
        assert_eq!(summary(&restored)[3], "패치 유지 여부: 확인 불가");
        // A folder that takes no write, and no folder at all: no room is said without one.
        let refused = Facts { write_test: WriteTest::Failed(Some(5)), ..home() };
        assert_eq!(
            summary(&refused)[..2],
            ["전체 검사 결과: 패치 불가", "패치 가능 여부: 불가능 (설치 폴더에 쓸 수 없음)"]
        );
        assert_eq!(
            summary(&locked_down()),
            ["전체 검사 결과: 확인 불가", "패치 가능 여부: 확인 불가", "패치 유지 여부: 확인 불가"]
        );
        // Every word a page shows for the patch is one the summary knows.
        for (facts, takes) in [
            (home(), Takes::Yes),
            (tight(200, room), Takes::Immediately),
            (refused, Takes::No),
            (locked_down(), Takes::Unknown),
        ] {
            assert_eq!(Takes::of(&verdict_of(&facts, &nobody())), takes);
        }
    }

    #[test]
    fn what_could_not_be_read_says_so_with_its_error_number_and_the_report_goes_on() {
        let report = checked(&locked_down(), &nobody(), &Head::Check);
        for (title, name, expected) in [
            ("PC 환경", "사용 장소", "확인 불가"),
            ("PC 환경", "디스크 유형", "확인 불가"),
            ("PC 환경", "운영체제", "확인 실패 · 64비트"),
            ("PC 환경", "CPU", "4코어 4스레드"),
            ("PC 환경", "메모리", "확인 실패"),
            ("PC 환경", "실행 권한", "확인 실패 (오류 5)"),
            ("PC 환경", "WebView2", "확인 실패 (오류 5)"),
            ("디스크 환경", "시스템", "확인 실패 (오류 21)"),
            ("게임 설치 상태", "게임 이름", "메이플스토리"),
            ("게임 설치 상태", "설치 경로", "없음"),
            ("게임 파일 검사", "검사 기준", "미실행"),
            ("네트워크 환경", "연결 방식", "확인 실패 (오류 1168)"),
            ("네트워크 환경", "다운로드 속도", "확인 실패 (연결 실패)"),
            ("네트워크 환경", "응답 지연 시간", "확인 실패 (연결 실패)"),
            ("성능 측정", "디스크 읽기 속도", "미측정"),
            ("패치 환경 검사", "설치 폴더 쓰기", "미실행"),
            ("패치 환경 검사", "잔여 파일", "없음"),
            (
                "환경 분석",
                "PC 유형 근거",
                "설치 폴더를 찾지 못함 · 시스템 디스크 정보 확인 실패|프로그램·드라이버 목록 확인 실패 (오류 5)",
            ),
            ("환경 분석", "유지 판정 근거", "프로그램·드라이버 목록 확인 실패 (오류 5)"),
            ("검사 기록", "PC 정보 전송", "없음"),
        ] {
            assert_eq!(said(&report, title, name).as_deref(), Some(expected), "{title} / {name} in\n{report}");
        }
        // Nothing follows a folder that is not there, and nothing a comparison that was not made.
        assert_eq!(rows(&report, "게임 설치 상태").len(), 2);
        assert_eq!(rows(&report, "게임 파일 검사").len(), 1);
        // Parts of a drive that Windows would not tell of are named one by one.
        let half = Volume {
            bus: Err(Unknown::Windows(1)),
            free: Err(Unknown::Windows(5)),
            kind: DriveKind::Removable,
            read_only: true,
            disk: Err(Unknown::Windows(1)),
            ..volume("D:", Bus::SATA, "", "NTFS", (9530, 0), 0)
        };
        let report = checked(&Facts { game_drive: Ok(half), ..home() }, &nobody(), &Head::Check);
        assert_eq!(
            said(&report, "디스크 환경", "D: 게임").unwrap(),
            "연결 방식 확인 실패 (오류 1) · NTFS · 이동식 드라이브|읽기 전용|여유 확인 실패 (오류 5) / 953.0 GB"
        );
        assert_eq!(said(&report, "디스크 환경", "디스크 구성").unwrap(), "확인 실패");
        assert_eq!(said(&report, "검사 요약", "패치 공간"), None);
        // What was found without the game's program saying its version, and with no
        // server to ask.
        let bare = Facts {
            game: Some(Game {
                file_version: Err(Unknown::Windows(1812)),
                patcher_log: None,
                files: Err(Unknown::Windows(5)),
                app_traces: false,
            }),
            sizes: Ok(Comparison { at: None, ..by_size(13) }),
            shelf: Err(Unknown::Http(503)),
            reads: Err(Unknown::Windows(32)),
            ..home()
        };
        let report = checked(&bare, &nobody(), &Head::Check);
        for (title, name, expected) in [
            ("게임 설치 상태", "현재 게임 버전", "일치 버전 없음"),
            ("게임 설치 상태", "최신 게임 버전", "확인 실패 (HTTP 503)"),
            ("게임 설치 상태", "설치 용량", "확인 실패 (오류 5)"),
            ("게임 설치 상태", "전체 파일 개수", "확인 실패 (오류 5)"),
            ("게임 파일 검사", "검사 대상 버전", "Ver 1.2.421 마이너버전(4) · 일치 버전 없음"),
            ("네트워크 환경", "다운로드 속도", "확인 실패 (HTTP 503)"),
            ("성능 측정", "디스크 읽기 속도", "확인 실패 (오류 32)"),
        ] {
            assert_eq!(said(&report, title, name).as_deref(), Some(expected), "{title} / {name} in\n{report}");
        }
        assert_eq!(said(&report, "성능 측정", "반복 읽기 속도"), None);
    }

    #[test]
    fn files_that_differ_or_are_missing_are_named_and_a_long_list_is_counted() {
        let names: Vec<String> = (0..25).map(|at| format!(r"Data\Map_{at:03}.wz")).collect();
        let locked = vec![("a.wz".to_owned(), Some(32)), ("b.wz".to_owned(), Some(32)), ("c.wz".to_owned(), Some(5))];
        let tally = Tally {
            same: 5,
            differs: named(&["Canvas.dll", "MapleStory.exe"]),
            missing: names,
            unreadable: locked,
            ..Tally::default()
        };
        let read = Comparison { at: None, ..by_content(tally, 3 * GIB, 12.0) };
        let facts = Facts { quick: Ok(read.clone()), ..home() };
        let report = checked(&facts, &nobody(), &Head::Check);
        let found = rows(&report, "게임 파일 검사");
        let row = |name: &str| found.iter().find(|(row, _)| row == name).map(|(_, lines)| lines.clone()).unwrap();
        assert_eq!(row("검사 기준"), ["파일 내용 비교 (일부)"]);
        assert_eq!(row("검사 대상 버전"), ["Ver 1.2.421 마이너버전(4) · 일치 버전 없음"]);
        assert_eq!(row("정상 파일"), ["5개"]);
        assert_eq!(row("변경 파일"), ["2개", "Canvas.dll, MapleStory.exe"]);
        // The first twenty, three to a line, and how many more there are.
        let missing = row("누락 파일");
        assert_eq!(missing[0], "25개");
        assert_eq!(missing[1], r"Data\Map_000.wz, Data\Map_001.wz, Data\Map_002.wz,");
        assert_eq!(missing.last().unwrap(), r"Data\Map_018.wz, Data\Map_019.wz 외 5개");
        assert_eq!(missing.len(), 8);
        assert!(!report.contains("Map_020"));
        // Files that could not be read, with the error numbers they failed with.
        assert_eq!(row("읽기 실패 파일"), ["3개 (오류 5, 32)", "a.wz, b.wz, c.wz"]);
        assert_eq!(row("검사 제외 파일"), ["13개"]);
        assert_eq!(row("검사 시간"), ["12초 · 3.0 GB"]);
        // The partial comparison says how many it read, and one that was stopped says so.
        let partly = Tally { same: 750, same_length: 222, unread: 240, ..Tally::default() };
        let stopped = Comparison { cut_short: true, ..by_content(partly, tenths_of_gb(58), 45.0) };
        let report = checked(&Facts { quick: Ok(stopped), ..home() }, &nobody(), &Head::Check);
        assert_eq!(said(&report, "게임 파일 검사", "정상 파일").unwrap(), "1,212개 (내용 750개 · 크기만 462개)");
        assert_eq!(said(&report, "게임 파일 검사", "검사 시간").unwrap(), "45초 · 5.8 GB · 중단됨");
        assert_eq!(said(&report, "게임 파일 검사", "변경 파일").unwrap(), "없음");
        // The verdict does not go by a comparison that was stopped, and neither does the
        // version that is said to be installed.
        assert_eq!(said(&report, "게임 설치 상태", "현재 게임 버전").unwrap(), "Ver 1.2.421 마이너버전(4)");
        // Several versions fit, and a list that is not the one built in.
        let open = Comparison {
            at: None,
            either: vec![State { version: 421, minor: 3 }, NEWEST],
            against: Some((NEWEST, Source::Server)),
            ..by_size(13)
        };
        let report = checked(&Facts { sizes: Ok(open), ..home() }, &nobody(), &Head::Check);
        assert_eq!(said(&report, "게임 파일 검사", "검사 기준").unwrap(), "파일 크기 비교 · 서버 목록");
        let either = "Ver 1.2.421 마이너버전(3), Ver 1.2.421 마이너버전(4)";
        assert_eq!(said(&report, "게임 파일 검사", "검사 대상 버전").unwrap(), either);
        assert_eq!(said(&report, "게임 설치 상태", "현재 게임 버전").unwrap(), either);
        let report = checked(&Facts { table_given: true, ..home() }, &nobody(), &Head::Check);
        assert_eq!(said(&report, "게임 파일 검사", "검사 기준").unwrap(), "파일 크기 비교 · 지정 목록");
    }

    #[test]
    fn version_the_registry_or_the_games_patcher_names_is_said_where_it_is_another() {
        let report = checked(&home(), &nobody(), &Head::Check);
        assert_eq!(rows(&report, "게임 설치 상태").len(), 6);
        let other = Facts {
            registry: Registry { install_dir: None, version_hint: Some(420) },
            game: Some(Game { patcher_log: Some("00421_03".to_owned()), ..home().game.unwrap() }),
            ..home()
        };
        let report = checked(&other, &nobody(), &Head::Check);
        assert_eq!(said(&report, "게임 설치 상태", "등록 버전").unwrap(), "420");
        assert_eq!(said(&report, "게임 설치 상태", "패치 기록 버전").unwrap(), "Ver 1.2.421 마이너버전(3)");
        let names: Vec<String> = rows(&report, "게임 설치 상태").into_iter().map(|(name, _)| name).collect();
        assert_eq!(
            names,
            [
                "게임 이름",
                "설치 경로",
                "현재 게임 버전",
                "최신 게임 버전",
                "등록 버전",
                "패치 기록 버전",
                "설치 용량",
                "전체 파일 개수"
            ]
        );
        // A server that has moved on is said in the row of its own.
        let behind =
            Facts { shelf: Ok(Shelf { newest: State { version: 422, minor: 1 }, ..shelf(20.0, 9.0) }), ..home() };
        let report = checked(&behind, &nobody(), &Head::Check);
        assert_eq!(said(&report, "게임 설치 상태", "최신 게임 버전").unwrap(), "Ver 1.2.422");
        assert_eq!(said(&report, "게임 설치 상태", "현재 게임 버전").unwrap(), "Ver 1.2.421 마이너버전(4)");
    }

    #[test]
    fn way_to_nexons_server_is_said_in_megabytes_a_second_and_in_milliseconds() {
        let network = |shelf: Fact<Shelf>| {
            let report = checked(&Facts { shelf, ..seat_by_default() }, &nobody(), &Head::Check);
            ["다운로드 속도", "응답 지연 시간"].map(|name| said(&report, "네트워크 환경", name).unwrap())
        };
        assert_eq!(network(Ok(shelf(15.0, 12.4))), ["15 MB/s", "12 ms"]);
        assert_eq!(network(Ok(shelf(4.26, 3.42))), ["4.3 MB/s", "3.4 ms"]);
        // No connection opened, while the files came: the one is said without the other.
        let unreached = Shelf { round_trip: Err(Unknown::NoConnection), ..shelf(15.0, 0.0) };
        assert_eq!(network(Ok(unreached)), ["15 MB/s", "확인 실패 (연결 실패)"]);
        // The check was told what the server has and asked nothing, or was told not to
        // ask: neither was measured, and that is no failure.
        let told = Shelf { sample: Err(Unknown::Skipped), round_trip: Err(Unknown::Skipped), ..shelf(1.0, 1.0) };
        assert_eq!(network(Ok(told)), ["미측정", "미측정"]);
        assert_eq!(network(Err(Unknown::Skipped)), ["미측정", "미측정"]);
        assert_eq!(network(Err(Unknown::NoAnswer)), ["확인 실패", "확인 실패"]);
        // The rows of the group, in their order; without a card that is connected, no speed.
        let report = checked(&seat_by_default(), &nobody(), &Head::Check);
        let names: Vec<String> = rows(&report, "네트워크 환경").into_iter().map(|(name, _)| name).collect();
        assert_eq!(names, ["연결 방식", "네트워크 속도", "다운로드 속도", "응답 지연 시간"]);
        let both = vec![
            Adapter { model: String::new(), wireless: false, speed: Some(2_500_000_000) },
            Adapter { model: String::new(), wireless: true, speed: None },
        ];
        let report = checked(&Facts { adapters: Ok(both), ..home() }, &nobody(), &Head::Check);
        assert_eq!(said(&report, "네트워크 환경", "연결 방식").unwrap(), "유선 LAN · 무선 Wi-Fi");
        assert_eq!(said(&report, "네트워크 환경", "네트워크 속도").unwrap(), "2.5 Gbps · 확인 실패");
        let report = checked(&Facts { adapters: Ok(Vec::new()), ..home() }, &nobody(), &Head::Check);
        assert_eq!(said(&report, "네트워크 환경", "연결 방식").unwrap(), "없음");
        assert_eq!(said(&report, "네트워크 환경", "네트워크 속도"), None);
    }

    #[test]
    fn steps_that_write_say_how_far_they_got_and_a_step_that_was_refused_says_why() {
        let began = [("C:", 154 * GIB), ("D:", 305 * GIB)];
        let whole = |gigabytes: u64| {
            let speeds = vec![410.0; gigabytes as usize];
            Landing {
                wanted: ROOM_NEEDED,
                ..wrote_at("D:", Place::Install, &began, GIB, &speeds, &vec![0; gigabytes as usize])
            }
        };
        let limit = |landing: Landing| {
            let report =
                checked(&Facts { limit: WriteLimit::Done(landing), ..seat_by_default() }, &nobody(), &Head::Check);
            said(&report, "패치 환경 검사", "한도 검사").unwrap()
        };
        // Ended by the user, on an error, and by the test itself for each of its reasons.
        assert_eq!(limit(whole(12)), "D: 12.0 GB · 410 MB/s|12.0 GB에서 중단 · 사용자 중단");
        assert_eq!(
            limit(Landing { error: Some(112), ..whole(21) }),
            "D: 21.0 GB · 410 MB/s|21.0 GB에서 중단 · 쓰기 실패 (오류 112)"
        );
        for (halt, why) in [
            (Halt::Drive, "여유 공간 부족"),
            (Halt::Memory, "메모리 부족"),
            (Halt::Slow, "속도 급감"),
            (Halt::TimedOut, "시간 초과"),
        ] {
            let said = limit(Landing { halted: Some(halt), ..whole(16) });
            assert_eq!(said, format!("D: 16.0 GB · 410 MB/s|16.0 GB에서 중단 · {why}"));
        }
        // Another volume went down with what was written: it is named twice over, as why
        // the test ended and with how much it lost.
        let mut elsewhere = Landing { halted: Some(Halt::Elsewhere), ..whole(28) };
        elsewhere.volumes[0].after_write = Some(126 * GIB);
        assert_eq!(
            limit(elsewhere),
            "D: 28.0 GB · 410 MB/s|28.0 GB에서 중단 · C: 여유 부족|쓰기 중 C: 여유 28.0 GB 감소"
        );
        // All of it written: nothing under the figures.
        let all = Landing { wanted: 4 * GIB, ..whole(4) };
        assert_eq!(limit(all), "D: 4.0 GB · 410 MB/s");
        let nothing = Landing { written: 0, halted: Some(Halt::Drive), steps: Vec::new(), ..whole(1) };
        assert_eq!(limit(nothing), "D: 쓰지 않음 · 여유 공간 부족");

        // Refused, each with its figures; left out where it was only not asked for.
        let refused = |limit: WriteLimit| {
            let report = checked(&Facts { limit, ..seat_by_default() }, &nobody(), &Head::Check);
            said(&report, "패치 환경 검사", "한도 검사")
        };
        let no_room = |free: Option<u64>| WriteLimit::NoRoom {
            volume: "G:".to_owned(),
            free: free.map(|gigabytes| gigabytes * GIB),
            writes: ROOM_NEEDED,
            floor: 4 * GIB,
        };
        assert_eq!(refused(no_room(Some(30))).unwrap(), "미실행 (G: 여유 30.0 GB / 필요 40.5 GB)");
        assert_eq!(refused(no_room(None)).unwrap(), "미실행 (G: 여유 확인 실패)");
        let no_memory = |available: Option<u64>| WriteLimit::NoMemory { available, wanted: 6 * GIB };
        assert_eq!(refused(no_memory(Some(9 * GIB / 2))).unwrap(), "미실행 (메모리 4.5 GB / 필요 6.0 GB)");
        assert_eq!(refused(no_memory(None)).unwrap(), "미실행 (메모리 확인 실패)");
        let no_place = |error: Option<i32>| WriteLimit::NoPlace { volume: "E:".to_owned(), error };
        assert_eq!(refused(no_place(Some(19))).unwrap(), "미실행 (E: 쓰기 실패, 오류 19)");
        assert_eq!(refused(no_place(None)).unwrap(), "미실행 (E: 쓰기 가능한 고정 볼륨 아님)");
        assert_eq!((refused(WriteLimit::Declined), refused(WriteLimit::NotPossible)), (None, None));

        // The write check: a volume that took no write has its error number, and files
        // that could not be taken away again are named with where they are.
        let probe = wrote_at("D:", Place::Install, &began, 256 * MIB, &[410.0; 4], &[0; 4]);
        let seat = Facts {
            landing: Asked::Done(Landing { deleted: false, ..probe }),
            other_landings: vec![
                Probed { volume: "E:".to_owned(), landing: Err(Some(19)) },
                Probed { volume: "F:".to_owned(), landing: Err(None) },
            ],
            write_test: WriteTest::LeftBehind(Some(32)),
            wrote: vec![
                Wrote::Leftovers { removed: 2, left: 1 },
                Wrote::EmptyFile { removed: false },
                Wrote::ProbeFile { bytes: GIB, removed: false },
                Wrote::OwnFolder {
                    volume: "C:".to_owned(),
                    place: Place::Temp,
                    bytes: GIB,
                    limit: false,
                    removed: false,
                },
                Wrote::OwnFolder { volume: "C:".to_owned(), place: Place::Temp, bytes: 0, limit: true, removed: true },
                Wrote::Report { file_name: "mfp-check-20261127-0119.txt".to_owned() },
            ],
            took: (5.2, 31.0),
            ..seat_by_default()
        };
        let report = checked(&seat, &nobody(), &Head::Check);
        for (title, name, expected) in [
            ("패치 환경 검사", "설치 폴더 쓰기", "성공"),
            ("패치 환경 검사", "임시 파일 삭제", "실패 (오류 32)"),
            (
                "패치 환경 검사",
                "잔여 파일",
                "이전 검사의 임시 파일 1개 · .mfp-check-*.tmp (설치 폴더)|mfp-check-tmp (C:)",
            ),
            ("패치 환경 검사", "D: 쓰기 검사", "1.0 GB · 410 MB/s"),
            ("패치 환경 검사", "E: 쓰기 검사", "실패 (오류 19)"),
            ("패치 환경 검사", "F: 쓰기 검사", "실패"),
            ("패치 환경 검사", "메모리 변화", "쓰기 중 감소 없음"),
            ("검사 기록", "검사 소요 시간", "31초 (기본 검사 5.2초)"),
            ("검사 기록", "D: 설치 폴더", "임시 파일 1.0 GB 생성 · 삭제 실패"),
            ("검사 기록", "C: 임시 폴더", "시스템 임시 폴더의 mfp-check-tmp에 1.0 GB 생성 · 삭제 실패"),
            ("검사 기록", "이전 검사 파일", "2개 삭제 · 1개 삭제 실패"),
            ("검사 기록", "검사 결과 저장", "실행 파일 폴더의 mfp-check-20261127-0119.txt"),
        ] {
            assert_eq!(said(&report, title, name).as_deref(), Some(expected), "{title} / {name} in\n{report}");
        }
        // The folder took no empty file: nothing was there to take away.
        let report = checked(&Facts { write_test: WriteTest::Failed(Some(5)), ..home() }, &nobody(), &Head::Check);
        assert_eq!(said(&report, "패치 환경 검사", "설치 폴더 쓰기").unwrap(), "실패 (오류 5)");
        assert_eq!(said(&report, "패치 환경 검사", "임시 파일 삭제"), None);
        assert_eq!(said(&report, "환경 분석", "패치 판정 근거").unwrap(), "빈 파일 생성 실패 (오류 5)");
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
        // A disk whose name marks it as made up, so that the verdict goes by the name.
        let hostile_volume = |letter: &str| Volume {
            file_system: Ok(hostile("NTFS")),
            label: format!("{COMPUTER}_{USER}_7391"),
            model: hostile("NSM Virtual Disk"),
            ..volume(letter, Bus::SATA, "", "", (9530, 1000), 0)
        };
        let names = |what: &str| vec![format!("{USER}-{what}.exe"), format!("{COMPUTER}{what}.sys")];
        let hostile_write = || Landing {
            written: GIB,
            wanted: 2 * GIB,
            seconds: 1.0,
            error: None,
            halted: Some(Halt::Elsewhere),
            steps: vec![
                Step { bytes: GIB / 2, seconds: 0.5, free: vec![Some(2 * GIB), Some(GIB)], memory: None },
                Step { bytes: GIB / 2, seconds: 0.5, free: vec![Some(GIB), Some(GIB)], memory: None },
            ],
            volumes: vec![
                Moved {
                    letter: format!(r"\\{COMPUTER}\{USER}"),
                    written_to: false,
                    before: Some(3 * GIB),
                    after_write: Some(GIB),
                    after_delete: Some(GIB),
                    later: None,
                },
                Moved {
                    letter: format!(r"\\{COMPUTER}\{USER}2"),
                    written_to: true,
                    before: Some(GIB),
                    after_write: Some(GIB),
                    after_delete: Some(GIB),
                    later: None,
                },
            ],
            memory: [None; 4],
            waited: 5.0,
            deleted: false,
            place: Place::Install,
        };
        let own = |place: Place| Wrote::OwnFolder {
            volume: format!(r"\\{COMPUTER}\{USER}3"),
            place,
            bytes: GIB,
            limit: false,
            removed: false,
        };
        let facts = Facts {
            windows: hostile("Windows"),
            cpu: hostile("CPU"),
            webview: Ok(Some(WebView { version: hostile("1.2.3.4"), for_all: false })),
            adapters: Ok(vec![Adapter { model: hostile("Realtek"), wireless: false, speed: None }]),
            system_drive: Ok(hostile_volume("C:")),
            how: How::Typed,
            install: Some(PathBuf::from(format!(r"{HOME}\Games\{COMPUTER}\Maple"))),
            registry: Registry { install_dir: Some(format!(r"c:\users\{USER}\Other\Maple")), version_hint: Some(420) },
            game_drive: Ok(Volume { disk: Ok(3), ..hostile_volume(&format!(r"\\{COMPUTER}\{USER}")) }),
            other_volumes: vec![hostile_volume("E:"), Volume { model: hostile("Disk"), ..hostile_volume("F:") }],
            game: Some(Game { patcher_log: Some(hostile("log")), ..home().game.unwrap() }),
            sizes: Ok(Comparison {
                tally: Tally {
                    differs: vec![format!(r"{HOME}\Data\{USER}.wz")],
                    missing: vec![format!(r"Data\{COMPUTER}.wz")],
                    unreadable: vec![(format!(r"{USER}\{ADDRESS}.wz"), Some(5))],
                    ..Tally::default()
                },
                ..by_size(13)
            }),
            programs: Ok(Programs {
                diskless: names("pfgame"),
                restore: names("restore"),
                seat: names("pmclient"),
                iscsi: names("iscsiprt"),
                netdisk: names("nxddsk"),
                spare: names("megasas"),
                hints: names("cache"),
                noted: names("wireguard"),
                game: names("MapleStory"),
            }),
            landing: Asked::Done(hostile_write()),
            other_landings: vec![
                Probed { volume: format!(r"\\{COMPUTER}\{USER}3"), landing: Ok(hostile_write()) },
                Probed { volume: format!(r"\\{COMPUTER}\{USER}4"), landing: Err(Some(5)) },
            ],
            limit: WriteLimit::Done(Landing { place: Place::Root, ..hostile_write() }),
            wrote: vec![Wrote::ProbeFile { bytes: GIB, removed: false }, own(Place::Root), own(Place::Temp)],
            ..home()
        };
        // The grounds of every answer name programs somewhere; all kinds of verdict are
        // gone through by taking the evidence away piece by piece, and the steps that
        // were refused name volumes too.
        let mut reports = vec![checked(&facts, &scrub, &Head::Check)];
        let mut fewer = facts.clone();
        for step in 0..6 {
            if let Ok(programs) = fewer.programs.as_mut() {
                match step {
                    0 => programs.diskless.clear(),
                    1 => programs.restore.clear(),
                    2 => programs.seat.clear(),
                    3 => programs.iscsi.clear(),
                    4 => programs.spare.clear(),
                    _ => programs.hints.clear(),
                }
            }
            reports.push(checked(&fewer, &scrub, &Head::Check));
        }
        let shared = format!(r"\\{COMPUTER}\{USER}");
        for limit in [
            WriteLimit::NoRoom { volume: shared.clone(), free: Some(GIB), writes: 2 * GIB, floor: GIB },
            WriteLimit::NoRoom { volume: shared.clone(), free: None, writes: 2 * GIB, floor: GIB },
            WriteLimit::NoPlace { volume: shared.clone(), error: Some(5) },
            WriteLimit::NoPlace { volume: shared.clone(), error: None },
        ] {
            reports.push(checked(&Facts { limit, ..facts.clone() }, &scrub, &Head::App("0.1.0".to_owned())));
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
            // What Windows handed in cannot break the report's lines or its columns either.
            assert!(report.lines().all(|line| width(line) <= WIDTH && !line.contains('\r')), "{report}");
        }
        // What is shown of each is there, with the names taken out of it.
        let first = &reports[0];
        assert_eq!(said(first, "게임 설치 상태", "설치 경로").unwrap(), r"%USERPROFILE%\Games\***\Maple");
        assert!(said(first, "PC 환경", "운영체제").unwrap().starts_with("Windows *** *** %USERPROFILE%\\secret"));
        assert!(said(first, "디스크 환경", "C: 시스템").unwrap().starts_with("NSM Virtual Disk *** ***"), "{first}");
        assert_eq!(said(first, "게임 파일 검사", "변경 파일").unwrap(), r"1개|%USERPROFILE%\Data\***.wz");
        assert_eq!(said(first, "게임 파일 검사", "누락 파일").unwrap(), r"1개|Data\***.wz");
        assert_eq!(said(first, "게임 파일 검사", "읽기 실패 파일").unwrap(), r"1개 (오류 5)|***\*.*.*.*.wz");
        assert!(said(first, "게임 설치 상태", "패치 기록 버전").unwrap().starts_with("log *** ***"), "{first}");
        assert!(said(first, "패치 환경 검사", "한도 검사").unwrap().contains(r"쓰기 중 \\***\*** 여유 2.0 GB 감소"));
        assert!(said(first, "패치 환경 검사", "잔여 파일").unwrap().contains(r"mfp-check-tmp (\\***\***3)"), "{first}");
        assert!(said(first, "환경 분석", "PC 유형 근거").unwrap().contains("***-pfgame.exe, ***pfgame.sys"), "{first}");
        assert!(reports[7].contains(r"미실행 (\\***\*** 여유 1.0 GB / 필요 3.0 GB)"), "{}", reports[7]);
    }

    #[test]
    fn test_world_is_named_as_the_test_world() {
        let at = State { version: 1208, minor: 2 };
        let facts = Facts {
            client: Server::Test,
            install: Some(PathBuf::from(r"D:\Games\MapleT")),
            registry: Registry::default(),
            sizes: Ok(Comparison { at: Some(at), against: Some((at, Source::Table)), ..by_size(3) }),
            shelf: Ok(Shelf { newest: at, ..shelf(20.0, 9.0) }),
            game: Some(Game { patcher_log: None, ..home().game.unwrap() }),
            ..home()
        };
        let report = checked(&facts, &nobody(), &Head::Check);
        for (name, expected) in [
            ("게임 이름", "메이플스토리 테스트월드"),
            ("설치 경로", r"D:\Games\MapleT"),
            ("현재 게임 버전", "Ver 1.2.208 마이너버전(2)"),
            ("최신 게임 버전", "Ver 1.2.208 마이너버전(2)"),
        ] {
            assert_eq!(said(&report, "게임 설치 상태", name).as_deref(), Some(expected), "{report}");
        }
        assert_eq!(said(&report, "게임 파일 검사", "검사 대상 버전").unwrap(), "Ver 1.2.208 마이너버전(2)");
    }

    #[test]
    fn where_the_pc_is_and_what_its_disk_is_follow_the_kind_of_pc() {
        let place = |facts: &Facts| {
            let report = checked(facts, &nobody(), &Head::Check);
            ["사용 장소", "디스크 유형", "게임 저장 위치"].map(|name| said(&report, "PC 환경", name).unwrap())
        };
        assert_eq!(place(&seat_by_default()), ["PC방", "노하드 (서버 디스크 방식)", "매장 서버 (네트워크)"]);
        assert_eq!(place(&home()), ["일반 PC", "PC 내장 디스크", "이 PC의 디스크"]);
        let with = |programs: Programs| Facts { programs: Ok(programs), ..home() };
        // A disk that is put back, at home and in a shop.
        let restore = Programs { restore: named(&["DFServ.exe"]), ..Programs::default() };
        assert_eq!(place(&with(restore.clone())), ["일반 PC", "복구 프로그램 사용", "이 PC의 디스크"]);
        let shop = Programs { seat: named(&["pmclient.exe"]), ..restore };
        assert_eq!(place(&with(shop))[0], "PC방");
        // A shop's program on disks of the PC: a PC room, and what its disk is is not told.
        let managed = Programs { seat: named(&["pmclient.exe"]), ..Programs::default() };
        assert_eq!(place(&with(managed)), ["PC방", "확인 불가", "확인 불가"]);
        assert_eq!(place(&locked_down()), ["확인 불가", "확인 불가", "확인 불가"]);
    }

    #[test]
    fn text_is_laid_out_in_the_columns_there_are() {
        assert_eq!([width("C: 시스템·게임"), width("디스크 읽기 속도"), width("→ …")], [14, 16, 3]);
        // A path keeps its end, and a name too long for its column its beginning as well.
        let long = r"D:\Program Files (x86)\Some Publisher\Games Of All Kinds\Nexon\Maple";
        assert_eq!(middle(long, ROOM), r"D:\Program Files (x86)\Some Publisher\Games Of All …\Maple");
        assert_eq!(width(&middle(long, ROOM)), ROOM);
        assert_eq!(middle(r"D:\Games\Maple", ROOM), r"D:\Games\Maple");
        assert_eq!(middle("메이플스토리테스트월드", 12), "메이…트월드");
        // Values side by side, broken where one ends; a list inside one, after a comma.
        let values = named(&[
            "Fibre Channel 디스크",
            "NSM 디스크",
            "PicaigreenClient.exe, PicaVncSvc.exe, pmclient.exe",
            "iScsiPrt",
        ]);
        assert_eq!(
            lay(&values),
            ["Fibre Channel 디스크 · NSM 디스크 · PicaigreenClient.exe,", "PicaVncSvc.exe, pmclient.exe · iScsiPrt"]
        );
        assert_eq!(lay(&named(&["", "가", "", "나"])), ["가 · 나"]);
        assert!(lay(&[]).is_empty());
        // One value that is too long for a line is broken between its words.
        let sentence = "가나다라 ".repeat(9);
        assert_eq!(
            lay(&[sentence.trim().to_owned()]),
            ["가나다라 가나다라 가나다라 가나다라 가나다라 가나다라", "가나다라 가나다라 가나다라"]
        );
        // The analysis keeps to two lines, and counts what it leaves out.
        let many: Vec<String> = (1..=9).map(|at| format!("근거가 되는 사실 {at}번")).collect();
        assert_eq!(
            grounds(&many),
            ["근거가 되는 사실 1번 · 근거가 되는 사실 2번", "근거가 되는 사실 3번 · 근거가 되는 사실 4번 외 5개"]
        );
        assert_eq!(grounds(&many[..4]).len(), 2);
    }

    #[test]
    fn processor_is_named_without_what_its_maker_pads_the_name_with() {
        for (raw, shown) in [
            ("13th Gen Intel(R) Core(TM) i5-13400F", "Intel Core i5-13400F"),
            ("11th Gen Intel(R) Core(TM) i7-11800H @ 2.30GHz", "Intel Core i7-11800H"),
            ("Intel(R) Core(TM) i5-10400 CPU @ 2.90GHz", "Intel Core i5-10400"),
            ("Intel(R) Xeon(R) CPU           E5520  @ 2.27GHz", "Intel Xeon E5520"),
            ("AMD Ryzen 5 5600X 6-Core Processor", "AMD Ryzen 5 5600X"),
            ("Intel(R) Core(TM) Ultra 7 155H", "Intel Core Ultra 7 155H"),
            ("", ""),
        ] {
            assert_eq!(cpu_name(raw), shown);
        }
    }

    #[test]
    fn volumes_that_share_a_disk_are_said_to() {
        let on_disks = |disks: &[(&str, Fact<u32>)]| {
            layout(&disks.iter().map(|(letter, disk)| ((*letter).to_owned(), *disk)).collect::<Vec<_>>()).join(" · ")
        };
        assert_eq!(on_disks(&[("C:", Ok(0)), ("D:", Ok(1)), ("S:", Ok(2))]), "C:, D:, S: 각각 별도 디스크");
        assert_eq!(on_disks(&[("C:", Ok(0)), ("G:", Ok(1)), ("D:", Ok(0))]), "C:, D: 같은 디스크 · G: 별도 디스크");
        assert_eq!(on_disks(&[("C:", Ok(0)), ("D:", Ok(0))]), "C:, D: 같은 디스크");
        assert_eq!(
            on_disks(&[("C:", Ok(0)), ("D:", Ok(1)), ("E:", Err(Unknown::Windows(1)))]),
            "C:, D: 각각 별도 디스크 · E: 확인 실패"
        );
        assert_eq!(on_disks(&[("C:", Ok(0)), ("D:", Err(Unknown::Windows(1)))]), "확인 실패");
        // One volume has nothing to share a disk with, and the row is left out.
        let one = volume("C:", Bus::NVME, "SAMSUNG MZVL21T0HCLR-00BL2", "NTFS", (9530, 2110), 0);
        let facts = Facts { system_drive: Ok(one.clone()), game_drive: Ok(one), ..home() };
        let report = checked(&facts, &nobody(), &Head::Check);
        assert_eq!(
            rows(&report, "디스크 환경"),
            [(
                "C: 시스템·게임".to_owned(),
                named(&["SAMSUNG MZVL21T0HCLR-00BL2 · NVMe · NTFS", "여유 211.0 / 953.0 GB"])
            )]
        );
    }
}
