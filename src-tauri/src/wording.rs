//! What the engine's problems and errors are called on the screen, what the app itself is
//! called, and how sizes and drives are written.

use std::path::Path;

use mfp_engine::Error;
use mfp_engine::preflight::Problem;

/// What the app is called: in a line of text, and as it stands on a title bar.
pub(crate) const APP_NAME: &str = "Maple Fast Patch";
pub(crate) const APP_TITLE: &str = "MAPLE FAST PATCH";

/// The drive a path is on, as `C:`. `None` for a path without a drive letter, such as a
/// folder shared by another computer: that computer's name has no place in a text that is
/// made to be passed on.
pub(crate) fn drive_letter(path: &Path) -> Option<String> {
    use std::path::{Component, Prefix};
    let Component::Prefix(prefix) = path.components().next()? else { return None };
    match prefix.kind() {
        Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
            Some(format!("{}:", char::from(letter).to_ascii_uppercase()))
        }
        _ => None,
    }
}

/// What is in the way, in a few words for a line on the ready screen.
pub(crate) fn describe_problem(problem: &Problem) -> String {
    match problem {
        Problem::NotAnInstallDir { missing, .. } => format!("{missing} 없음"),
        Problem::SpaceUnknown { volume, .. } => format!("{volume} 여유 공간 확인 불가"),
        Problem::NotEnoughSpace { volume, needed, available } => {
            let (needed, available) = need_and_free(*needed, *available);
            format!("{volume} 필요 {needed} · 여유 {available}")
        }
        Problem::NotWritable { .. } => "파일 쓰기 불가".to_owned(),
        // The line asks for the game to be closed, and nothing stands at its right: which of
        // its programs are open is for the log, where the engine names them.
        Problem::GameRunning { .. } => String::new(),
        Problem::FilesLocked { files } => format!("사용 중인 파일 {}개", files.len()),
        Problem::OutsideInstall { dir } => dir.display().to_string(),
        Problem::AlreadyRunning => "패치 진행 중".to_owned(),
        Problem::LeftoverWork { .. } => "이전 패치 미완료".to_owned(),
        Problem::UnreadableJournal { .. } => "작업 기록 손상".to_owned(),
        Problem::ImmediateNeedsNtfs { .. } => "즉시 패치 불가".to_owned(),
    }
}

/// What is in the way, as the sentence a dialog shows. Worded the way the game's own
/// launcher words its messages. `game` is what the client's game is called, for a sentence
/// that names it.
pub(crate) fn sentence(game: &str, problem: &Problem) -> String {
    match problem {
        Problem::NotAnInstallDir { .. } => not_the_games_folder(game),
        Problem::SpaceUnknown { .. } => "디스크 여유 공간을 확인할 수 없습니다.".to_owned(),
        Problem::NotEnoughSpace { volume, needed, available } => {
            let (needed, available) = need_and_free(*needed, *available);
            format!("디스크 공간이 부족합니다.\n{volume} 여유 공간을 {needed} 이상 확보해 주세요. (현재 {available})")
        }
        Problem::NotWritable { .. } => "설치 경로에 파일을 쓸 수 없습니다.".to_owned(),
        // Which of the game's programs are open is in the log. Few know them by name, and
        // what there is to do is the same for all of them.
        Problem::GameRunning { .. } => quit_first(game),
        Problem::FilesLocked { .. } => "다른 프로그램이 게임 파일을 사용 중입니다.".to_owned(),
        Problem::OutsideInstall { dir } => {
            format!("설치 경로 밖으로 연결된 폴더가 있습니다. ({})", dir.display())
        }
        Problem::AlreadyRunning => "이미 패치가 진행 중입니다.".to_owned(),
        Problem::LeftoverWork { .. } => "이전 패치가 완료되지 않았습니다.".to_owned(),
        // What could not be read of the record is in the log. Nothing this app does puts
        // such a folder right, and the second line says what does.
        Problem::UnreadableJournal { .. } => {
            "이전 패치 작업 기록이 손상되었습니다.\n공식 런처로 게임을 복구해 주세요.".to_owned()
        }
        // Which file system the drive has instead is in the log.
        Problem::ImmediateNeedsNtfs { .. } => "즉시 패치는 NTFS 드라이브에서만 사용할 수 있습니다.".to_owned(),
    }
}

/// What is said of a folder that is not the install folder of the game called `game`: one
/// without that game's program in it, which the other client's folder is as well.
pub(crate) fn not_the_games_folder(game: &str) -> String {
    format!("{game} 설치 경로가 아닙니다.")
}

/// What is said when the install folder of the game called `game` is not found.
pub(crate) fn no_folder_of(game: &str) -> String {
    format!("{game} 설치 경로를 찾을 수 없습니다.")
}

/// What is asked of the user while the game called `game` is open, which is in the way of
/// a patch: the line of the ready screen says it, and so does a dialog. Both of the games'
/// names end in a vowel, which is what the `를` after them goes by.
pub(crate) fn quit_first(game: &str) -> String {
    format!("{game}를 종료한 후 실행해 주세요.")
}

/// What the user is asked as they switch 패치 후 게임 자동 실행 on in an app that runs without
/// administrator rights; see [`crate::rights`].
pub(crate) const ASKS_TO_RESTART_WITH_RIGHTS: &str =
    "게임 자동 실행에는 관리자 권한이 필요합니다.\n관리자 권한으로 다시 시작하시겠습니까?";

/// A time something takes, as roughly as it is known: in minutes from 55 seconds on, and
/// below that rounded up to ten seconds. The page words the time a run still needs by the
/// same rule.
fn about(seconds: f64) -> String {
    if seconds >= 55.0 {
        format!("약 {}분", ((seconds / 60.0).round() as u64).max(1))
    } else {
        format!("약 {}초", ((seconds / 10.0).ceil() as u64 * 10).max(10))
    }
}

/// What the user is asked before the game's files are checked (see [`crate::diagnosis`]),
/// with about how long that takes on this PC: `seconds`, where the diagnosis timed the
/// drive. A player is asked in a player's words. What the check holds the files against is
/// in the report, for whoever it is copied to.
pub(crate) fn asks_to_check_files(seconds: Option<f64>) -> String {
    let asks = "게임 파일을 검사하시겠습니까?";
    match seconds.map(about) {
        Some(about) if about.ends_with('분') => format!("{asks}\n{about}이 걸립니다."),
        Some(about) => format!("{asks}\n{about}가 걸립니다."),
        None => asks.to_owned(),
    }
}

/// Why a run ended, as the sentence a dialog shows, and any lines that go under it. `game`
/// is what the client's game is called.
pub(crate) fn describe_error(game: &str, err: &Error) -> (String, Vec<String>) {
    match err {
        // A handful of names, and how many more there are; the log has them all.
        Error::OriginalMismatch(files) => {
            const SHOWN: usize = 6;
            let mut lines: Vec<String> = files.iter().take(SHOWN).cloned().collect();
            if files.len() > SHOWN {
                lines.push(format!("외 {}개", files.len() - SHOWN));
            }
            ("게임 파일이 패치와 일치하지 않습니다.".to_owned(), lines)
        }
        // What is in the way is the message itself; any further problems follow it.
        Error::Blocked(problems) => {
            let mut lines = problems.iter().map(|problem| sentence(game, problem));
            (lines.next().unwrap_or_else(|| "패치를 진행할 수 없습니다.".to_owned()), lines.collect())
        }
        // Which two patch files they are is in the log. Only the one that began in a folder
        // can finish it.
        Error::PinnedToOtherPatch { .. } => {
            (OTHER_PATCH.to_owned(), vec!["같은 패치 파일이 없으면 게임을 다시 설치해 주세요.".to_owned()])
        }
        Error::NotPublished => ("패치 파일이 아직 등록되지 않았습니다.".to_owned(), Vec::new()),
        // The system's own wording of a network error helps nobody on screen; it is in the log.
        // What the screen says is where the trouble is. The server answered with something
        // other than the file, or stopped sending in the middle of it: that is the server's.
        // Anything else is somewhere on the way there, and the user's own line is the part
        // of the way they can look at.
        Error::Download { status, detail } => {
            let servers = status.is_some() || detail == WENT_SILENT;
            let where_to_look =
                if servers { "서버 응답 오류" } else { "네트워크 연결 상태를 확인해 주세요." };
            ("패치 파일을 다운로드할 수 없습니다.".to_owned(), vec![where_to_look.to_owned()])
        }
        Error::NotAPatchFile => (format!("{game} 패치 파일이 아닙니다."), Vec::new()),
        // A run ends this way before it changes anything, so the game's own launcher can
        // still bring the game up to date.
        Error::UnknownForm(_) => {
            ("지원하지 않는 패치 형식입니다.".to_owned(), vec!["공식 런처로 업데이트해 주세요.".to_owned()])
        }
        Error::CorruptPatch(_) => ("패치 파일 검사에 실패했습니다.".to_owned(), Vec::new()),
        Error::NewFileMismatch(name) => ("새 게임 파일 검사에 실패했습니다.".to_owned(), vec![name.clone()]),
        Error::UnsafePath(name) => ("패치 파일에 잘못된 경로가 있습니다.".to_owned(), vec![name.clone()]),
        // Only the system's own reason; which step and file it was is in the log.
        Error::Io { source, .. } => ("파일 처리 중 오류가 발생했습니다.".to_owned(), vec![source.to_string()]),
        Error::Stopped => ("패치가 일시정지되었습니다.".to_owned(), Vec::new()),
    }
}

/// What is said of a patch file that is not the one a folder was left partly new by.
const OTHER_PATCH: &str = "진행 중인 패치와 다른 패치 파일입니다.";

/// What is asked before a patch is fetched from an address the user gave that is on a
/// server of Nexon's and not among the patch files of the game called `game`: the app
/// cannot vouch for what is there being a patch for this game.
pub(crate) fn asks_about_address(game: &str) -> String {
    format!("{game} 패치 주소가 아닙니다.\n이 주소로 패치하시겠습니까?")
}

/// What the engine says of a download whose server was reached and then sent nothing more.
const WENT_SILENT: &str = "the server went silent";

/// What the footer says of a folder in which an apply was cut off, until it is put back.
pub(crate) const PUT_BACK_FIRST: &str = "이전 패치 복구 대기";

/// What the footer of the ready screen says of an app that is too old. It is said and no
/// more: a start is turned down for nothing of the kind.
pub(crate) const UPDATE_FIRST: &str = "최신 버전으로 업데이트해 주세요.";

/// What is said where a client has no install folder to do anything in: to whoever asks for
/// something to be done there.
pub(crate) const NO_FOLDER: &str = "설치 경로를 찾을 수 없습니다.";

/// What is said when an apply that was cut off could not be undone, and under it what is in
/// the way, where the engine names something that is: a folder of the run's own that leads
/// somewhere else, say, which nothing but taking the link away puts right.
pub(crate) fn not_restored(game: &str, err: &Error) -> String {
    let mut lines = vec!["이전 패치를 복구하지 못했습니다.".to_owned()];
    if let Error::Blocked(problems) = err {
        lines.extend(problems.iter().map(|problem| sentence(game, problem)));
    }
    lines.join("\n")
}

/// Why a run in a folder ended, as [`describe_error`] words it, with one case of its own.
///
/// A folder that a run left partly new (`half_patched`) takes the rest of that patch only.
/// The engine turns any other patch file down as work that is left over, which is how the
/// folder looks to it. To the person who gave its address it is the patch that is the wrong
/// one, and the right one is still in the folder: there is nothing to tell them to do
/// about the game.
pub(crate) fn describe_run_error(game: &str, err: &Error, half_patched: bool) -> (String, Vec<String>) {
    match err {
        Error::Blocked(problems)
            if half_patched && problems.iter().any(|problem| matches!(problem, Problem::LeftoverWork { .. })) =>
        {
            (OTHER_PATCH.to_owned(), Vec::new())
        }
        _ => describe_error(game, err),
    }
}

/// Whether a run that ended with `err` was turned down, as opposed to having failed at its
/// work: a check that has to pass before the folder may be changed did not pass, or the
/// patch file is not the one the folder can take.
pub(crate) fn turned_down(err: &Error) -> bool {
    matches!(err, Error::Blocked(_) | Error::PinnedToOtherPatch { .. })
}

/// Why the app could not work out which patch fits, in a few words for the status line.
pub(crate) fn describe_check_error(err: &Error) -> String {
    match err {
        Error::OriginalMismatch(_) => "게임 파일 불일치".to_owned(),
        Error::Download { .. } => "서버 요청 실패".to_owned(),
        Error::UnknownForm(_) => "패치 형식 미지원".to_owned(),
        // A check is only ever short of room on the drive of this app's cache.
        Error::Blocked(problems) => match problems.first() {
            Some(Problem::NotEnoughSpace { volume, .. }) => format!("{volume} 여유 공간 부족").trim_start().to_owned(),
            _ => "패치 확인 불가".to_owned(),
        },
        _ => "패치 확인 불가".to_owned(),
    }
}

/// Bytes to the units sizes are written in.
const MB: u64 = 1 << 20;
const GB: u64 = 1 << 30;

/// A size to a tenth of a GB: `34.5 GB`.
pub(crate) fn gib(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / GB as f64)
}

/// A size in whole GB, where that is as close as anyone needs it: `476 GB`.
pub(crate) fn whole_gb(bytes: u64) -> String {
    format!("{} GB", (bytes as f64 / GB as f64).round())
}

/// What a drive has to have free and what it has, each to a tenth of a GB: the first
/// rounded up and the second down, so that a drive a few megabytes short does not read as
/// having exactly what is asked of it.
fn need_and_free(needed: u64, available: u64) -> (String, String) {
    let tenths = |bytes: u64| bytes as f64 * 10.0 / GB as f64;
    (format!("{:.1} GB", tenths(needed).ceil() / 10.0), format!("{:.1} GB", tenths(available).floor() / 10.0))
}

/// A size in the unit that suits it.
pub(crate) fn size_label(bytes: u64) -> String {
    if bytes >= GB { gib(bytes) } else { format!("{} MB", bytes / MB) }
}

/// A size in whole megabytes, as the screen writes it: `1,229 MB`.
pub(crate) fn megabytes(bytes: u64) -> String {
    let whole = ((bytes as f64 / (1u64 << 20) as f64).round() as u64).to_string();
    let mut grouped = String::new();
    for (at, digit) in whole.chars().enumerate() {
        if at > 0 && (whole.len() - at).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    format!("{grouped} MB")
}

/// How fast `bytes` came in over `seconds`: `86.0 MB/s`.
pub(crate) fn speed(bytes: u64, seconds: f64) -> String {
    format!("{:.1} MB/s", bytes as f64 / MB as f64 / seconds)
}

/// How long something took, to the second: `1분 35초`, and `59초` under a minute.
pub(crate) fn minutes_and_seconds(seconds: u64) -> String {
    if seconds >= 60 { format!("{}분 {}초", seconds / 60, seconds % 60) } else { format!("{seconds}초") }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// What the live game is called.
    const GAME: &str = "메이플스토리";

    fn left_over() -> Error {
        Error::Blocked(vec![Problem::LeftoverWork { dir: PathBuf::from("work") }])
    }

    fn no_room(then: Vec<Problem>) -> Error {
        let short =
            Problem::NotEnoughSpace { volume: "C:".to_owned(), needed: 37_151_000_000, available: 17_072_000_000 };
        Error::Blocked(std::iter::once(short).chain(then).collect())
    }

    /// A server that was reached and then went silent is the server's trouble, like one
    /// that answers with something other than the file. A line that leads nowhere is the
    /// user's to look at.
    #[test]
    fn a_download_that_failed_says_whether_to_look_at_the_server_or_at_the_line() {
        let failed =
            |status, detail: &str| describe_error(GAME, &Error::Download { status, detail: detail.to_owned() });
        let at_the_server = ("패치 파일을 다운로드할 수 없습니다.".to_owned(), vec!["서버 응답 오류".to_owned()]);
        assert_eq!(failed(Some(503), "unexpected answer"), at_the_server);
        assert_eq!(failed(None, "the server went silent"), at_the_server);
        let on_the_way =
            ("패치 파일을 다운로드할 수 없습니다.".to_owned(), vec!["네트워크 연결 상태를 확인해 주세요.".to_owned()]);
        assert_eq!(failed(None, "io: failed to lookup address information"), on_the_way);
        assert_eq!(failed(None, "connection lost: timed out"), on_the_way);
        // A check says the same of both in the footer.
        for detail in ["the server went silent", "io: timeout"] {
            let err = Error::Download { status: None, detail: detail.to_owned() };
            assert_eq!(describe_check_error(&err), "서버 요청 실패");
        }
    }

    /// The record an apply keeps so that it can be undone cannot be read. Wherever that is
    /// in the way, it is said in the same two lines, and a line of the ready screen has
    /// the short form.
    #[test]
    fn a_record_of_an_apply_that_cannot_be_read_is_said_to_be_damaged_with_what_puts_the_game_right() {
        let work = PathBuf::from(r"C:\Nexon\Maple\.maple-fast-patch");
        let unreadable = Problem::UnreadableJournal { dir: work, detail: r#"line 3: "garbage""#.to_owned() };
        assert_eq!(describe_problem(&unreadable), "작업 기록 손상");
        let lines = "이전 패치 작업 기록이 손상되었습니다.\n공식 런처로 게임을 복구해 주세요.";
        assert_eq!(sentence(GAME, &unreadable), lines);
        // Neither the folder nor what stood in the record is for the screen.
        for shown in [describe_problem(&unreadable), sentence(GAME, &unreadable)] {
            assert!(!shown.contains("garbage") && !shown.contains("Nexon"), "{shown}");
        }
        // A run that is turned down for it, and a delete that is.
        let turned = Error::Blocked(vec![unreadable]);
        assert_eq!(describe_run_error(GAME, &turned, false), (lines.to_owned(), Vec::new()));
        assert!(turned_down(&turned));
        // An apply that could not be undone for it says so, and why.
        assert_eq!(not_restored(GAME, &turned), format!("이전 패치를 복구하지 못했습니다.\n{lines}"));
    }

    /// A work folder that is a link to somewhere else, or holds one, is turned down where
    /// a run begins, where an apply that was cut off is to be undone, and where what a run
    /// left is to be deleted. Each says which folder it is.
    #[test]
    fn a_work_folder_that_leads_elsewhere_is_named_wherever_it_is_in_the_way() {
        let work = PathBuf::from(r"C:\Nexon\Maple\.maple-fast-patch");
        let linked = Error::Blocked(vec![Problem::OutsideInstall { dir: work }]);
        let sentence = r"설치 경로 밖으로 연결된 폴더가 있습니다. (C:\Nexon\Maple\.maple-fast-patch)";
        // A run that is turned down for it, and a delete that is: the sentence itself.
        assert_eq!(describe_run_error(GAME, &linked, false), (sentence.to_owned(), Vec::new()));
        assert!(turned_down(&linked));
        // An apply that could not be undone for it: that, and why.
        assert_eq!(not_restored(GAME, &linked), format!("이전 패치를 복구하지 못했습니다.\n{sentence}"));
        // Any other reason is the system's own, and stays in the log.
        let denied = Error::Io { what: "rename Base.wz".to_owned(), source: std::io::Error::other("denied") };
        assert_eq!(not_restored(GAME, &denied), "이전 패치를 복구하지 못했습니다.");
    }

    #[test]
    fn another_patch_file_for_a_half_patched_folder_is_called_that() {
        let (message, details) = describe_run_error(GAME, &left_over(), true);
        assert_eq!(message, "진행 중인 패치와 다른 패치 파일입니다.");
        // The right file is still in the folder. Nothing is said about installing again.
        assert!(details.is_empty());
    }

    #[test]
    fn work_left_in_a_whole_folder_keeps_its_own_sentence() {
        assert_eq!(describe_run_error(GAME, &left_over(), false), describe_error(GAME, &left_over()));
        assert_eq!(describe_error(GAME, &left_over()).0, "이전 패치가 완료되지 않았습니다.");
    }

    #[test]
    fn a_half_patched_folder_changes_no_other_sentence() {
        let pinned = Error::PinnedToOtherPatch { pinned: "a".to_owned(), offered: "b".to_owned() };
        for err in [no_room(Vec::new()), pinned, Error::NotAPatchFile, Error::Stopped] {
            assert_eq!(describe_run_error(GAME, &err, true), describe_error(GAME, &err));
        }
    }

    #[test]
    fn a_patch_file_the_server_replaced_says_so_with_what_is_left_to_do() {
        let pinned = Error::PinnedToOtherPatch { pinned: "a".to_owned(), offered: "b".to_owned() };
        let (message, details) = describe_error(GAME, &pinned);
        assert_eq!(message, "진행 중인 패치와 다른 패치 파일입니다.");
        assert_eq!(details, ["같은 패치 파일이 없으면 게임을 다시 설치해 주세요."]);
        // Neither of the two ids is for the screen.
        assert!(!format!("{message}{details:?}").contains(['a', 'b']));
    }

    #[test]
    fn no_room_says_what_is_needed_and_what_is_there() {
        let (message, details) = describe_error(GAME, &no_room(Vec::new()));
        assert_eq!(message, "디스크 공간이 부족합니다.\nC: 여유 공간을 34.6 GB 이상 확보해 주세요. (현재 15.8 GB)");
        assert!(details.is_empty());
    }

    /// The immediate way was chosen on a drive that does not take it. The run is turned
    /// down for that alone, before anything is fetched, whatever room the drive has.
    #[test]
    fn the_immediate_way_on_a_drive_that_does_not_take_it_is_turned_down_for_that_alone() {
        let exfat = Problem::ImmediateNeedsNtfs { file_system: "exFAT".to_owned() };
        assert_eq!(describe_problem(&exfat), "즉시 패치 불가");
        let refused = Error::Blocked(vec![exfat]);
        let (message, details) = describe_error(GAME, &refused);
        assert_eq!(message, "즉시 패치는 NTFS 드라이브에서만 사용할 수 있습니다.");
        assert!(details.is_empty() && turned_down(&refused));
        assert!(!message.contains("exFAT"));
    }

    /// The live game's sentences are the ones they have always been; the test world's name
    /// the test world, so that a folder or a file of the other client is called by what it
    /// is not.
    #[test]
    fn a_sentence_that_names_the_game_names_the_clients_own() {
        let (live, test) = (mfp_kms::LIVE.title, mfp_kms::TEST.title);
        let no_game = Problem::NotAnInstallDir { dir: PathBuf::from("D:\\Games"), missing: "MapleStoryT.exe" };
        assert_eq!(sentence(live, &no_game), "메이플스토리 설치 경로가 아닙니다.");
        assert_eq!(sentence(test, &no_game), "메이플스토리 테스트월드 설치 경로가 아닙니다.");
        assert_eq!(no_folder_of(live), "메이플스토리 설치 경로를 찾을 수 없습니다.");
        assert_eq!(no_folder_of(test), "메이플스토리 테스트월드 설치 경로를 찾을 수 없습니다.");
        assert_eq!(describe_error(live, &Error::NotAPatchFile).0, "메이플스토리 패치 파일이 아닙니다.");
        assert_eq!(describe_error(test, &Error::NotAPatchFile).0, "메이플스토리 테스트월드 패치 파일이 아닙니다.");
        // A problem a run is turned down for is worded by the same rule.
        let blocked = Error::Blocked(vec![no_game]);
        assert_eq!(describe_error(test, &blocked).0, "메이플스토리 테스트월드 설치 경로가 아닙니다.");
        // The request to close the game names the game, and none of its programs: the line
        // of the ready screen and the dialog say the one sentence, and nothing stands at
        // the line's right.
        let open = Problem::GameRunning { names: vec!["BlackCipher64.aes".to_owned(), "MapleStoryT.exe".to_owned()] };
        assert_eq!(sentence(live, &open), "메이플스토리를 종료한 후 실행해 주세요.");
        assert_eq!(sentence(test, &open), "메이플스토리 테스트월드를 종료한 후 실행해 주세요.");
        assert_eq!((quit_first(live), describe_problem(&open)), (sentence(live, &open), String::new()));
        assert_eq!(describe_error(test, &Error::Blocked(vec![open])), (quit_first(test), Vec::new()));
        // Nothing else names the game.
        let held = Problem::FilesLocked { files: vec![PathBuf::from("Base.wz")] };
        assert_eq!(sentence(live, &held), sentence(test, &held));
    }

    #[test]
    fn the_user_is_asked_before_the_games_files_are_checked_with_about_how_long_it_takes() {
        // Rounded as the page rounds the time a run still needs: up to ten seconds, and
        // in minutes from 55 seconds on.
        let table = [
            (4.0, "약 10초가"),
            (30.0, "약 30초가"),
            (31.0, "약 40초가"),
            (54.9, "약 60초가"),
            (55.0, "약 1분이"),
            (89.0, "약 1분이"),
            (90.0, "약 2분이"),
            (586.0, "약 10분이"),
        ];
        for (seconds, about) in table {
            let asked = asks_to_check_files(Some(seconds));
            assert_eq!(asked, format!("게임 파일을 검사하시겠습니까?\n{about} 걸립니다."), "{seconds}");
        }
        // Where the drive was not timed, nothing is said of the time.
        assert_eq!(asks_to_check_files(None), "게임 파일을 검사하시겠습니까?");
    }

    #[test]
    fn sizes_are_written_in_whole_megabytes_with_the_thousands_set_apart() {
        assert_eq!(megabytes(0), "0 MB");
        assert_eq!(megabytes(524_287), "0 MB");
        assert_eq!(megabytes(524_288), "1 MB");
        assert_eq!(megabytes(999 << 20), "999 MB");
        assert_eq!(megabytes(1_288_490_189), "1,229 MB");
        assert_eq!(megabytes(34_359_738_368), "32,768 MB");
        assert_eq!(megabytes(1_234_567 << 20), "1,234,567 MB");
    }

    #[test]
    fn a_run_is_turned_down_by_a_check_or_by_the_wrong_patch_file_and_fails_otherwise() {
        let pinned = Error::PinnedToOtherPatch { pinned: "a".to_owned(), offered: "b".to_owned() };
        assert!(turned_down(&left_over()) && turned_down(&no_room(Vec::new())) && turned_down(&pinned));
        let io = Error::Io { what: "place a file".to_owned(), source: std::io::Error::other("held") };
        for failed in [Error::NewFileMismatch("Map.wz".to_owned()), Error::OriginalMismatch(Vec::new()), io] {
            assert!(!turned_down(&failed), "{failed}");
        }
        assert!(!turned_down(&Error::Stopped));
    }
}
