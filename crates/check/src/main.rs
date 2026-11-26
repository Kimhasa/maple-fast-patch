//! The diagnostic as a program: one exe that needs no install, no window and no rights,
//! and runs from a double click or from a stick.
//!
//!     mfp-check [--folder <install-dir>] [--server live|test]
//!               [--full yes|no] [--write-probe yes|no] [--reads yes|no]
//!               [--no-write-test] [--offline] [--probe-mb <megabytes>] [--table <file>]
//!               [--copy] [--save] [--no-wait]
//!
//! Without arguments it finds the game, asks before the steps that take long or write,
//! prints the report, and then offers to copy it or to save it beside the program.
//!
//! - `--folder` names the install folder. Nothing is looked for then: not where the
//!   registry says, not in the usual place, not on the drives.
//! - `--full`, `--write-probe` and `--reads` answer the three questions beforehand: the
//!   full comparison, the write probe, and reading while the game runs.
//! - `--no-write-test` leaves out the empty file, `--offline` leaves out Nexon's server,
//!   `--probe-mb` sets the size of the probe file.
//! - `--table` names a table of known file states to go by in place of the one built in:
//!   one that the `known_files` program of `mfp-tools` made after a newer patch.
//! - `--copy` and `--save` copy and save the report without asking, and `--no-wait` ends
//!   the program when it is done.
//!
//! What it is doing goes to the error output, so that the standard output holds the report
//! and nothing else.

#[cfg(not(windows))]
fn main() {
    eprintln!("This program runs on Windows only.");
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    console::main()
}

#[cfg(windows)]
mod console {
    use std::io::{IsTerminal, Write};
    use std::path::PathBuf;
    use std::process::ExitCode;
    use std::sync::atomic::{AtomicBool, Ordering};

    use mfp_check::diagnose::{self, Choice, Desk, Options};
    use mfp_check::facts::{Facts, Unknown, Wrote};
    use mfp_check::known::Table;
    use mfp_check::privacy::Scrub;
    use mfp_check::{deliver, report};
    use mfp_kms::Server;

    /// Set when the user wants out: Ctrl+C, or the window was closed.
    static STOP: AtomicBool = AtomicBool::new(false);
    /// Set once nothing of this program is left in the install folder.
    static TIDY: AtomicBool = AtomicBool::new(true);

    /// What `--help` prints.
    const USAGE: &str = "Maple Fast Patch 진단
사용법: mfp-check [옵션]
  --folder <경로>        설치 폴더 지정(자동으로 찾지 않음)
  --server live|test     본섭 또는 테섭 지정
  --full yes|no          전체 대조(게임 파일 전체 읽기)
  --write-probe yes|no   쓰기 위치 확인(설치 폴더에 임시 파일 512 MB 쓰기)
  --reads yes|no         게임 실행 중 읽기 측정·파일 대조
  --no-write-test        쓰기 시험 생략
  --offline              넥슨 패치 서버 확인 생략
  --probe-mb <MB>        쓰기 위치 확인에 쓰는 임시 파일 크기
  --table <파일>         내장 표 대신 사용할 파일 상태 표
  --copy                 결과를 클립보드에 복사
  --save                 결과를 실행 파일 폴더에 저장
  --no-wait              끝나면 바로 종료";

    /// What the arguments ask for, beyond how the diagnostic is run.
    #[derive(Debug, Default, PartialEq, Eq)]
    struct After {
        copy: bool,
        save: bool,
        no_wait: bool,
    }

    /// Reads the arguments. `Err` holds what is wrong with them.
    fn arguments(args: impl Iterator<Item = String>) -> Result<(Options, After), String> {
        let mut options = Options::default();
        let mut after = After::default();
        let mut args = args;
        while let Some(arg) = args.next() {
            let mut value = |what: &str| args.next().ok_or(format!("{arg}: {what} 값이 필요합니다."));
            let choice = |value: String| match value.as_str() {
                "yes" => Ok(Choice::Yes),
                "no" => Ok(Choice::No),
                other => Err(format!("{other}: yes 또는 no로 입력해 주세요.")),
            };
            match arg.as_str() {
                "--folder" => options.folder = Some(PathBuf::from(value("폴더")?)),
                "--server" => {
                    let key = value("live 또는 test")?;
                    options.server =
                        Some(Server::from_key(&key).ok_or(format!("{key}: live 또는 test로 입력해 주세요."))?);
                }
                "--full" => options.full = choice(value("yes 또는 no")?)?,
                "--write-probe" => options.write_probe = choice(value("yes 또는 no")?)?,
                "--reads" => options.heavy_reads = choice(value("yes 또는 no")?)?,
                "--no-write-test" => options.write_test = false,
                "--offline" => options.online = false,
                "--probe-mb" => {
                    let megabytes: u64 =
                        value("크기")?.parse().map_err(|_| "--probe-mb: 숫자로 입력해 주세요.".to_owned())?;
                    // From one megabyte to four gigabytes: a probe, not a way to fill a drive.
                    options.probe_bytes = megabytes.clamp(1, 4096) << 20;
                }
                "--table" => {
                    let file = value("표 파일")?;
                    let text = std::fs::read_to_string(&file)
                        .map_err(|_| "--table: 표 파일을 읽을 수 없습니다.".to_owned())?;
                    let table =
                        Table::parse(&text).map_err(|_| "--table: 표 파일 형식이 올바르지 않습니다.".to_owned())?;
                    options.table = Some(table);
                }
                "--copy" => after.copy = true,
                "--save" => after.save = true,
                "--no-wait" => after.no_wait = true,
                other => return Err(format!("{other}: 알 수 없는 옵션입니다.")),
            }
        }
        Ok((options, after))
    }

    /// The console the program runs in.
    struct Console {
        /// Somebody is there to answer.
        answers: bool,
    }

    impl Console {
        fn read_line(&self) -> Option<String> {
            if !self.answers || STOP.load(Ordering::Relaxed) {
                return None;
            }
            let mut line = String::new();
            match std::io::stdin().read_line(&mut line) {
                Ok(read) if read > 0 => Some(line.trim().to_owned()),
                _ => None,
            }
        }

        fn ask(&self, question: &str) -> Option<String> {
            if !self.answers {
                return None;
            }
            eprint!("{question} ");
            let _ = std::io::stderr().flush();
            self.read_line()
        }
    }

    impl Desk for Console {
        fn status(&mut self, text: &str) {
            eprintln!("  {text}");
        }

        fn confirm(&mut self, question: &str) -> bool {
            // With nobody there to answer, nothing is asked and the answer is no.
            if !self.answers {
                return false;
            }
            let said = self.ask(&format!("{question} (Y/N)")).unwrap_or_default().to_lowercase();
            // `ㅛ` is what the Y key gives while the keyboard is set to Korean.
            let yes = ["y", "yes", "ㅛ", "예", "네"].contains(&said.as_str());
            if !yes {
                eprintln!("  건너뜁니다.");
            }
            yes
        }

        fn answer(&mut self, question: &str) -> Option<String> {
            self.ask(question).filter(|line| !line.is_empty())
        }
    }

    /// Called by Windows, on a thread of its own, when the user presses Ctrl+C or closes
    /// the window. The work is told to stop, which is when it removes what it wrote.
    unsafe extern "system" fn on_control(kind: u32) -> i32 {
        const CTRL_C: u32 = 0;
        const CTRL_BREAK: u32 = 1;
        STOP.store(true, Ordering::SeqCst);
        if kind != CTRL_C && kind != CTRL_BREAK {
            // The window is going. Windows ends the program when this returns, or five
            // seconds from now: the work gets that long to take its file away.
            for _ in 0..40 {
                if TIDY.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        1
    }

    /// Says that something went wrong inside the program and keeps the window open, where
    /// a double-clicked console would vanish with the message.
    fn stay_open_on_a_fault() {
        std::panic::set_hook(Box::new(|fault| {
            let place = fault.location().map_or(String::new(), |at| format!(" ({}:{})", at.file(), at.line()));
            eprintln!("진단 도구에 문제가 발생했습니다.{place}");
            if std::io::stdin().is_terminal() {
                eprintln!("Enter 키를 누르면 종료합니다.");
                let _ = std::io::stdin().read_line(&mut String::new());
            }
        }));
    }

    pub(super) fn main() -> ExitCode {
        stay_open_on_a_fault();
        // SAFETY: the function that is registered lives as long as the program and only
        // touches the two flags above.
        unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(on_control), 1) };

        let mut console = Console { answers: std::io::stdin().is_terminal() };
        if std::env::args().skip(1).any(|arg| matches!(arg.as_str(), "--help" | "-h" | "/?")) {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        let (options, after) = match arguments(std::env::args().skip(1)) {
            Ok(read) => read,
            Err(wrong) => {
                eprintln!(
                    "{wrong}
--help로 사용법을 확인해 주세요."
                );
                wait(&console, false);
                return ExitCode::from(2);
            }
        };

        eprintln!("Maple Fast Patch 진단을 시작합니다.");
        TIDY.store(false, Ordering::SeqCst);
        let (mut facts, scrub) = diagnose::run(&options, &mut console, &STOP);
        TIDY.store(true, Ordering::SeqCst);
        if STOP.load(Ordering::Relaxed) {
            eprintln!("진단을 중단했습니다.");
            return ExitCode::from(1);
        }

        println!("{}", report::render(&facts, &scrub));
        hand_over(&mut facts, &scrub, &console, &after);
        ExitCode::SUCCESS
    }

    /// Copies and saves the report as the arguments or the user ask.
    fn hand_over(facts: &mut Facts, scrub: &Scrub, console: &Console, after: &After) {
        let mut copied = false;
        if after.save {
            save(facts, scrub);
        }
        if after.copy {
            copied = copy(facts, scrub);
        }
        if after.no_wait {
            return;
        }
        // No answer at all, as from a console that is gone, ends it like Enter does.
        while let Some(said) = console.ask("\n[C] 클립보드에 복사  [S] 파일로 저장  [Enter] 종료:") {
            match said.to_lowercase().as_str() {
                // `ㅊ` and `ㄴ` are what the two keys give while the keyboard is set to Korean.
                "c" | "ㅊ" => copied = copy(facts, scrub),
                "s" | "ㄴ" => {
                    // A copy made before the save says nothing of the saved file yet.
                    if save(facts, scrub) && copied {
                        copy(facts, scrub);
                    }
                }
                "" => break,
                _ => {}
            }
        }
    }

    fn failed(why: Unknown) -> String {
        match why {
            Unknown::Windows(code) => format!("(오류 {code})"),
            _ => String::new(),
        }
    }

    /// Puts the report onto the clipboard, and says whether it is there.
    fn copy(facts: &Facts, scrub: &Scrub) -> bool {
        let copied = deliver::copy(&report::render(facts, scrub));
        match copied {
            Ok(()) => eprintln!("클립보드에 복사했습니다."),
            Err(why) => eprintln!("클립보드에 복사하지 못했습니다.{}", failed(why)),
        }
        copied.is_ok()
    }

    /// Saves the report beside the program. The saved text says of itself that it was
    /// saved, so the file is among what the report lists as written.
    fn save(facts: &mut Facts, scrub: &Scrub) -> bool {
        let Some(folder) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(PathBuf::from)) else {
            eprintln!("진단 결과를 저장하지 못했습니다.");
            return false;
        };
        let file_name = deliver::file_name(facts.when);
        let already = facts.wrote.iter().any(|wrote| matches!(wrote, Wrote::Report { .. }));
        if !already {
            facts.wrote.push(Wrote::Report { file_name: file_name.clone() });
        }
        match deliver::save(&report::render(facts, scrub), &folder, facts.when) {
            Ok(_) => {
                eprintln!("진단 결과를 저장했습니다. 실행 파일 폴더의 {file_name}");
                true
            }
            Err(why) => {
                if !already {
                    facts.wrote.pop();
                }
                eprintln!("진단 결과를 저장하지 못했습니다.{}", failed(why));
                false
            }
        }
    }

    /// Keeps the window open until Enter is pressed.
    fn wait(console: &Console, no_wait: bool) {
        if !no_wait {
            let _ = console.ask("Enter 키를 누르면 종료합니다.");
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn read(args: &[&str]) -> Result<(Options, After), String> {
            arguments(args.iter().map(|arg| (*arg).to_owned()))
        }

        #[test]
        fn no_arguments_ask_before_every_step_that_takes_long_or_writes() {
            let (options, after) = read(&[]).unwrap();
            assert_eq!(options, Options::default());
            assert_eq!(
                (options.full, options.write_probe, options.heavy_reads),
                (Choice::Ask, Choice::Ask, Choice::Ask)
            );
            assert!(options.write_test && options.online && options.folder.is_none());
            assert_eq!(options.probe_bytes, 512 << 20);
            assert_eq!(after, After::default());
        }

        #[test]
        fn arguments_answer_the_questions_beforehand() {
            let (options, after) = read(&[
                "--folder",
                r"C:\MapleFastPatchTest\Maple",
                "--server",
                "test",
                "--full",
                "yes",
                "--write-probe",
                "no",
                "--reads",
                "yes",
                "--no-write-test",
                "--offline",
                "--probe-mb",
                "64",
                "--copy",
                "--save",
                "--no-wait",
            ])
            .unwrap();
            assert_eq!(options.folder, Some(PathBuf::from(r"C:\MapleFastPatchTest\Maple")));
            assert_eq!(options.server, Some(Server::Test));
            assert_eq!(
                (options.full, options.write_probe, options.heavy_reads),
                (Choice::Yes, Choice::No, Choice::Yes)
            );
            assert!(!options.write_test && !options.online);
            assert_eq!(options.probe_bytes, 64 << 20);
            assert_eq!(after, After { copy: true, save: true, no_wait: true });
        }

        #[test]
        fn arguments_that_make_no_sense_are_refused() {
            for wrong in [
                &["--folder"][..],
                &["--full", "maybe"],
                &["--server", "both"],
                &["--probe-mb", "many"],
                &["--everything"],
                &[r"C:\MapleFastPatchTest\Maple"],
            ] {
                assert!(read(wrong).is_err(), "{wrong:?} was accepted");
            }
            // A probe is never larger than four gigabytes, and never nothing.
            assert_eq!(read(&["--probe-mb", "999999"]).unwrap().0.probe_bytes, 4096 << 20);
            assert_eq!(read(&["--probe-mb", "0"]).unwrap().0.probe_bytes, 1 << 20);
        }
    }
}
