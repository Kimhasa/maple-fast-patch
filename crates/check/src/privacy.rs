//! Keeps what names this PC or the person at it out of everything the diagnostic shows.
//!
//! A report is made to be passed on. So every piece of text that comes from Windows or from
//! a folder goes through a [`Scrub`] before it is shown: the name of the user and of the
//! computer are taken out wherever they stand, a folder under a user's own folder loses the
//! part that names the user, and anything that reads like the address of a network card or
//! of a computer is covered. Serial numbers are not covered here because they are never
//! read in the first place.

use std::path::Path;

/// What stands in the place of a user's own folder.
pub const HOME: &str = "%USERPROFILE%";
/// What stands in the place of a name that is not shown.
const HIDDEN: &str = "***";
/// Holds the place of [`HOME`] while names are taken out, so that a user called `user` does
/// not take a piece out of `%USERPROFILE%` itself. A character no text from Windows holds.
const HOME_MARK: char = '\u{E000}';

/// What must not be shown on this PC, and the way to take it out of a text.
#[derive(Debug, Clone, Default)]
pub struct Scrub {
    /// The names of the user and of the computer.
    names: Vec<String>,
    /// The user's own folder, in every spelling it was found in.
    homes: Vec<String>,
}

impl Scrub {
    /// A scrub for a PC whose user and computer are called `names` and whose user's own
    /// folder is at `homes`.
    pub fn new<N, H>(names: N, homes: H) -> Scrub
    where
        N: IntoIterator<Item = String>,
        H: IntoIterator<Item = String>,
    {
        let tidy = |list: Vec<String>| {
            let mut list: Vec<String> =
                list.into_iter().map(|item| item.trim().to_owned()).filter(|item| !item.is_empty()).collect();
            // The longest first: a name that is the beginning of another must not leave the
            // rest of the longer one standing.
            list.sort_by_key(|item| std::cmp::Reverse(item.chars().count()));
            list.dedup();
            list
        };
        let homes = homes.into_iter().map(|home| home.trim_end_matches(['\\', '/']).to_owned()).collect();
        Scrub { names: tidy(names.into_iter().collect()), homes: tidy(homes) }
    }

    /// A text that came from Windows or from a folder, as it may be shown.
    pub fn text(&self, raw: &str) -> String {
        let mut text: String = raw.chars().filter(|c| *c != HOME_MARK && !c.is_control()).collect();
        for home in &self.homes {
            for form in [home.replace('\\', "\\\\"), home.replace('\\', "/"), home.clone()] {
                // Only where the folder's name ends there: `C:\Users\an` is not `C:\Users\anna`.
                text =
                    replace(&text, &form, &HOME_MARK.to_string(), |_, next| !next.is_some_and(char::is_alphanumeric));
            }
        }
        text = without_profiles(&text);
        for name in &self.names {
            text = without_name(&text, name);
        }
        without_addresses(&text).replace(HOME_MARK, HOME)
    }

    /// A path as it may be shown.
    pub fn path(&self, path: &Path) -> String {
        // Windows gives the real place of a folder with `\\?\` in front, which says nothing.
        let shown = path.display().to_string();
        match (shown.strip_prefix(r"\\?\UNC\"), shown.strip_prefix(r"\\?\")) {
            (Some(shared), _) => self.text(&format!(r"\\{shared}")),
            (None, Some(local)) => self.text(local),
            (None, None) => self.text(&shown),
        }
    }

    /// The name of a volume as it may be shown: a shop numbers its drives and its seats, so
    /// every digit is covered as well.
    pub fn label(&self, raw: &str) -> String {
        self.text(raw).chars().map(|c| if c.is_numeric() { '#' } else { c }).collect()
    }
}

/// A letter as it is compared: `A` and `a` are one letter.
fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Puts `with` wherever `wanted` stands in `text`, in whatever case its letters are, and
/// where `fits` agrees to what stands before and after it.
fn replace(text: &str, wanted: &str, with: &str, fits: impl Fn(Option<char>, Option<char>) -> bool) -> String {
    let chars: Vec<char> = text.chars().collect();
    let wanted: Vec<char> = wanted.chars().map(fold).collect();
    if wanted.is_empty() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < chars.len() {
        let end = at + wanted.len();
        let found = end <= chars.len()
            && chars[at..end].iter().map(|c| fold(*c)).eq(wanted.iter().copied())
            && fits(at.checked_sub(1).map(|before| chars[before]), chars.get(end).copied());
        if found {
            out.push_str(with);
            at = end;
        } else {
            out.push(chars[at]);
            at += 1;
        }
    }
    out
}

/// Takes a name out of a text. A name of three letters or more is taken out wherever it
/// stands, also inside a longer word. A shorter one only where it stands by itself: a user
/// called `pc` would otherwise take a piece out of every other word.
fn without_name(text: &str, name: &str) -> String {
    let short = name.chars().count() < 3;
    let alone = |c: Option<char>| !c.is_some_and(char::is_alphanumeric);
    replace(text, name, HIDDEN, |before, after| !short || (alone(before) && alone(after)))
}

/// The folders Windows keeps the users' own folders in.
const PROFILE_ROOTS: [&str; 2] = ["users", "documents and settings"];

/// Covers the own folder of any user, whoever it is: `C:\Users\<name>` becomes [`HOME`],
/// and what follows the name stays.
fn without_profiles(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let separator = |c: char| c == '\\' || c == '/';
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    'next: while at < chars.len() {
        // A drive letter, a colon, and separators.
        let drive = chars[at].is_ascii_alphabetic() && chars.get(at + 1) == Some(&':');
        let mut root = at + 2;
        while drive && chars.get(root).copied().is_some_and(separator) {
            root += 1;
        }
        if drive && root > at + 2 {
            for folder in PROFILE_ROOTS {
                let end = root + folder.chars().count();
                let named = end <= chars.len() && chars[root..end].iter().map(|c| fold(*c)).eq(folder.chars());
                let mut name = end;
                while named && chars.get(name).copied().is_some_and(separator) {
                    name += 1;
                }
                if !named || name == end {
                    continue;
                }
                // The name runs to the next separator, or to something no name can hold.
                let mut after = name;
                while chars.get(after).is_some_and(|c| !separator(*c) && !r#"<>:"|?*"#.contains(*c)) {
                    after += 1;
                }
                if after > name {
                    out.push(HOME_MARK);
                    at = after;
                    continue 'next;
                }
            }
        }
        out.push(chars[at]);
        at += 1;
    }
    out
}

/// Covers what reads like the address of a network card (`00-1A-2B-3C-4D-5E`) or of a
/// computer (`192.168.0.15`, `fe80::1`).
fn without_addresses(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let hex = |at: usize| chars.get(at).is_some_and(char::is_ascii_hexdigit);
    let digit = |at: usize| chars.get(at).is_some_and(char::is_ascii_digit);
    let alone_before = |at: usize| at == 0 || !chars[at - 1].is_alphanumeric();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < chars.len() {
        if alone_before(at) {
            // Six pairs of hexadecimal digits with one kind of separator between them.
            let card = [':', '-'].into_iter().any(|between| {
                (0..6).all(|pair| hex(at + pair * 3) && hex(at + pair * 3 + 1))
                    && (0..5).all(|pair| chars.get(at + pair * 3 + 2) == Some(&between))
                    && !chars.get(at + 17).is_some_and(|c| c.is_alphanumeric())
            });
            if card {
                out.push_str("**-**-**-**-**-**");
                at += 17;
                continue;
            }
            // Four numbers with dots between them.
            let mut end = at;
            let mut parts = 0;
            while parts < 4 {
                let start = end;
                while digit(end) && end - start < 3 {
                    end += 1;
                }
                if end == start {
                    break;
                }
                parts += 1;
                if parts < 4 {
                    if chars.get(end) != Some(&'.') {
                        break;
                    }
                    end += 1;
                }
            }
            if parts == 4 && !digit(end) && (at == 0 || chars[at - 1] != '.') {
                out.push_str("*.*.*.*");
                at = end;
                continue;
            }
            // Groups of hexadecimal digits with colons between them: with a gap (`::`), or
            // six groups and more.
            let mut end = at;
            while chars.get(end).is_some_and(|c| c.is_ascii_hexdigit() || *c == ':') {
                end += 1;
            }
            let run: String = chars[at..end].iter().collect();
            let colons = run.matches(':').count();
            if run.chars().any(|c| c != ':') && ((run.contains("::") && colons >= 2) || colons >= 5) {
                out.push_str("*:*:*");
                at = end;
                continue;
            }
        }
        out.push(chars[at]);
        at += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scrub() -> Scrub {
        Scrub::new(["Minsu".to_owned(), "DESKTOP-7QK2M".to_owned()], [r"C:\Users\Minsu".to_owned()])
    }

    #[test]
    fn users_own_folder_is_covered_in_every_spelling() {
        let scrub = scrub();
        assert_eq!(scrub.text(r"C:\Users\Minsu\Games\Maple"), r"%USERPROFILE%\Games\Maple");
        assert_eq!(scrub.text(r"c:\users\MINSU\Games"), r"%USERPROFILE%\Games");
        assert_eq!(scrub.text("C:/Users/Minsu/Games"), "%USERPROFILE%/Games");
        assert_eq!(scrub.text(r"C:\\Users\\Minsu\\Games"), r"%USERPROFILE%\\Games");
        assert_eq!(scrub.path(Path::new(r"\\?\C:\Users\Minsu\Maple")), r"%USERPROFILE%\Maple");
        // A folder that only begins alike belongs to somebody else, who is covered as well.
        assert_eq!(scrub.text(r"C:\Users\Minsuk\x"), r"%USERPROFILE%\x");
    }

    #[test]
    fn own_folder_of_any_other_user_is_covered_too() {
        let scrub = scrub();
        assert_eq!(scrub.text(r"D:\Users\anna\Maple\Data"), r"%USERPROFILE%\Maple\Data");
        assert_eq!(scrub.text(r"C:\Documents and Settings\anna\Maple"), r"%USERPROFILE%\Maple");
        assert_eq!(scrub.text(r"C:\Users\anna"), "%USERPROFILE%");
        // A name with spaces in it runs to the next separator.
        assert_eq!(scrub.text(r"C:\Users\Anna Lee\Maple"), r"%USERPROFILE%\Maple");
        // Nothing to cover where no name follows, and nothing in a folder that is no user's.
        assert_eq!(scrub.text(r"C:\Users\"), r"C:\Users\");
        assert_eq!(scrub.text(r"D:\Nexon\Maple"), r"D:\Nexon\Maple");
        assert_eq!(scrub.text(r"D:\UsersGames\Maple"), r"D:\UsersGames\Maple");
    }

    #[test]
    fn names_of_the_user_and_the_computer_are_taken_out_wherever_they_stand() {
        let scrub = scrub();
        assert_eq!(scrub.text(r"D:\Games\minsu\Maple"), r"D:\Games\***\Maple");
        assert_eq!(scrub.text("minsu-cache.exe"), "***-cache.exe");
        assert_eq!(scrub.text("Disk of DESKTOP-7QK2M"), "Disk of ***");
        assert_eq!(scrub.text("xMINSUx"), "x***x");
    }

    #[test]
    fn short_name_is_taken_out_only_where_it_stands_by_itself() {
        let scrub = Scrub::new(["pc".to_owned()], [r"C:\Users\pc".to_owned()]);
        assert_eq!(scrub.text(r"C:\Users\pc\Maple"), r"%USERPROFILE%\Maple");
        assert_eq!(scrub.text("PC-01 pcbang.exe"), "***-01 pcbang.exe");
    }

    #[test]
    fn user_whose_name_is_part_of_the_placeholder_leaves_it_whole() {
        let scrub = Scrub::new(["user".to_owned(), "profile".to_owned()], [r"C:\Users\user".to_owned()]);
        assert_eq!(scrub.text(r"C:\Users\user\Maple of user"), r"%USERPROFILE%\Maple of ***");
    }

    #[test]
    fn longer_of_two_names_is_taken_out_whole() {
        let scrub = Scrub::new(["anna".to_owned(), "annabel".to_owned()], Vec::new());
        assert_eq!(scrub.text("annabel and anna"), "*** and ***");
    }

    #[test]
    fn what_reads_like_an_address_is_covered() {
        let scrub = scrub();
        assert_eq!(scrub.text("Realtek 00-1A-2B-3C-4D-5E"), "Realtek **-**-**-**-**-**");
        assert_eq!(scrub.text("card 00:1a:2b:3c:4d:5e."), "card **-**-**-**-**-**.");
        assert_eq!(scrub.text("at 192.168.0.15 now"), "at *.*.*.* now");
        assert_eq!(scrub.text("fe80::1ff:fe23:4567:890a"), "*:*:*");
        assert_eq!(scrub.text("2001:db8:0:0:0:0:2:1"), "*:*:*");
        // A drive, a time and a model are none of these.
        for plain in [r"C:\Nexon\Maple", "12:34:56", "SAMSUNG MZVL21T0HCLR-00BL2", "Core(TM) i7-11800H @ 2.30GHz"] {
            assert_eq!(scrub.text(plain), plain);
        }
    }

    #[test]
    fn name_of_a_volume_loses_its_digits_and_the_names() {
        let scrub = scrub();
        assert_eq!(scrub.label("GAME01"), "GAME##");
        assert_eq!(scrub.label("DESKTOP-7QK2M_D"), "***_D");
        assert_eq!(scrub.label("게임 2번"), "게임 #번");
    }

    #[test]
    fn nothing_to_hide_changes_nothing() {
        let scrub = Scrub::new(Vec::new(), Vec::new());
        assert_eq!(scrub.text(r"D:\Nexon\Maple · NTFS"), r"D:\Nexon\Maple · NTFS");
        // Line breaks and the like never come out of a text from Windows.
        assert_eq!(scrub.text("two\r\nlines"), "twolines");
    }
}
