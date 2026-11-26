//! Which of the two clients a program is run for.
//!
//! The tools do not know the games by name either. They ask `mfp-kms`, which does, and hand
//! the engine the programs of the client that was chosen with `--server`.

use mfp_engine::preflight::Programs;
use mfp_kms::{Client, Server};

/// The programs of a client, as the engine asks for them.
pub fn programs(client: &Client) -> Programs {
    Programs { game: client.game_program, launcher: client.launcher }
}

/// Takes `--server live|test` out of a program's arguments. Without it the live client is
/// meant.
pub fn take_server(args: &mut Vec<String>) -> Result<Server, String> {
    let Some(at) = args.iter().position(|arg| arg == "--server") else { return Ok(Server::Live) };
    let key = args.get(at + 1).cloned().ok_or("--server needs live or test")?;
    args.drain(at..=at + 1);
    Server::from_key(&key).ok_or(format!("unknown server {key}: live or test"))
}

/// Whether a path lies under a folder called `Nexon`, which is where the real games live.
/// The programs that write refuse such a path.
pub fn under_nexon(path: &std::path::Path) -> bool {
    path.components().any(|part| part.as_os_str().eq_ignore_ascii_case("nexon"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn server_is_taken_out_of_the_arguments_and_is_the_live_one_when_not_named() {
        let mut args = arguments(&["make", "--server", "test", "folder"]);
        assert_eq!(take_server(&mut args), Ok(Server::Test));
        assert_eq!(args, ["make", "folder"]);
        assert_eq!(take_server(&mut args), Ok(Server::Live));
        assert_eq!(args, ["make", "folder"]);
        assert_eq!(take_server(&mut arguments(&["--server", "live"])), Ok(Server::Live));
        assert!(take_server(&mut arguments(&["make", "--server"])).is_err());
        assert!(take_server(&mut arguments(&["--server", "both"])).is_err());
    }

    #[test]
    fn programs_are_those_of_the_client_and_real_game_folders_are_told() {
        for server in Server::ALL {
            let client = server.client();
            assert_eq!(programs(client), Programs { game: client.game_program, launcher: client.launcher });
        }
        assert_ne!(programs(&mfp_kms::LIVE), programs(&mfp_kms::TEST));

        use std::path::Path;
        assert!(under_nexon(Path::new(r"C:\Nexon\Maple")) && under_nexon(Path::new(r"D:\games\NEXON\MapleT\Data")));
        assert!(!under_nexon(Path::new(r"C:\MapleFastPatchTest\Bench")) && !under_nexon(Path::new(r"C:\NexonX\Maple")));
    }
}
