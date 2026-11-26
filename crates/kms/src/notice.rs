//! Maintenance of the game, as Nexon itself tells it.
//!
//! Two public sources say when the game is closed, and neither is one of the game's servers:
//!
//! - The inspection service the game's own site asks. It answers with the latest maintenance:
//!   when it begins, when it ends, and the line the site shows meanwhile
//!   (`[패치중] 11/19(목) ver1.2.421 클라이언트 패치(04:00~10:00)`).
//! - The notice board. A maintenance is announced there with the day and the hours in its
//!   title. An extension is written into the title (`(연장)`, with the new hours), and when
//!   the maintenance is over Nexon puts 완료 in front: `[패치완료] 11/19(목) ver1.2.421 …`.
//!
//! The service knows the hours as figures; the board knows what the service does not: that
//! a maintenance is really over, and of the short maintenance the service never lists.
//!
//! The test world has a board of its own, written the same way but for a few words
//! (`[점검종료]`, `전체 테스트월드 채널 점검`), and the service does not list it. It is also
//! closed altogether between its operating periods, which its pages say in a panel at the side.

use std::time::{SystemTime, UNIX_EPOCH};

/// Where Nexon says when one client's game is closed.
#[derive(Debug)]
pub struct Sources {
    /// The inspection service, where it lists this client's maintenance.
    pub inspection: Option<Inspection>,
    /// The board with the maintenance notices.
    pub board_url: &'static str,
}

/// The inspection service, and how it is asked for the latest maintenance.
#[derive(Debug)]
pub struct Inspection {
    pub url: &'static str,
    pub action: &'static str,
    pub request: &'static str,
}

/// The sources for the live game.
pub static LIVE: Sources = Sources {
    inspection: Some(Inspection {
        url: "https://api.maplestory.nexon.com/soap/maplestory.asmx",
        action: "https://api.maplestory.nexon.com/soap/GetInspectionInfo",
        request: concat!(
            r#"<?xml version="1.0" encoding="utf-8"?>"#,
            r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body>"#,
            r#"<GetInspectionInfo xmlns="https://api.maplestory.nexon.com/soap/" />"#,
            r#"</soap:Body></soap:Envelope>"#
        ),
    }),
    board_url: "https://maplestory.nexon.com/News/Notice/Inspection",
};

/// The sources for the test world. The inspection service does not list it.
pub static TEST: Sources =
    Sources { inspection: None, board_url: "https://maplestory.nexon.com/Testworld/News/Inspect" };

/// A moment in Korean time, in minutes since 1970. Both sources state Korean time, whatever
/// the clock of this PC is set to.
pub type Minutes = i64;

/// Now, in Korean time.
pub fn now() -> Minutes {
    let utc = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64);
    (utc + 9 * 3600) / 60
}

/// The time of day of a moment, as a clock shows it: `04:05`.
pub fn clock(at: Minutes) -> String {
    let of_day = at.rem_euclid(1440);
    format!("{:02}:{:02}", of_day / 60, of_day % 60)
}

/// A maintenance the inspection service lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub begins: Minutes,
    pub ends: Minutes,
    /// The line the game's site shows while it lasts.
    pub text: String,
}

/// The maintenance in an answer of the inspection service. The service lists the latest
/// one, also long after it is over.
pub fn read_inspection(xml: &str) -> Vec<Planned> {
    // The answer also describes its own layout, with elements of the same name; the records
    // themselves are the ones that carry attributes or come bare.
    let field = |record: &str, name: &str| -> Option<String> {
        let open = format!("<{name}>");
        let from = record.find(&open)? + open.len();
        Some(plain(&record[from..from + record[from..].find(&format!("</{name}>"))?]))
    };
    let mut planned = Vec::new();
    let mut rest = xml;
    while let Some(at) = rest.find("<InspectionInfo") {
        rest = &rest[at + "<InspectionInfo".len()..];
        let Some(end) = rest.find("</InspectionInfo>") else { break };
        let record = &rest[..end];
        rest = &rest[end..];
        let times = field(record, "startDateTime").zip(field(record, "endDateTime"));
        if let Some((begins, ends)) = times.and_then(|(begins, ends)| stamp(&begins).zip(stamp(&ends)))
            && ends > begins
        {
            planned.push(Planned { begins, ends, text: field(record, "strObstacleContents").unwrap_or_default() });
        }
    }
    planned
}

/// Korean time of a stamp such as `2026-11-19T04:00:00+09:00`.
fn stamp(text: &str) -> Option<Minutes> {
    let (date, time) = text.trim().split_once('T')?;
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let (month, day): (u32, u32) = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    let (hour, minute): (i64, i64) = (time.get(0..2)?.parse().ok()?, time.get(3..5)?.parse().ok()?);
    // The distance from UTC, where the stamp states one. The service states Korean time.
    let zone = time.get(5..).and_then(|rest| rest.find(['+', '-', 'Z']).map(|at| &rest[at..]));
    let ahead = match zone {
        Some("Z") => 0,
        Some(zone) => {
            let (hours, minutes) = zone[1..].split_once(':')?;
            let distance = hours.parse::<i64>().ok()? * 60 + minutes.parse::<i64>().ok()?;
            if zone.starts_with('-') { -distance } else { distance }
        }
        None => 9 * 60,
    };
    Some(days_from_civil(year, month, day) * 1440 + hour * 60 + minute - ahead + 9 * 60)
}

/// One line of the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub id: u32,
    pub title: String,
}

/// The notices on the first page of a board, newest first. Both boards are read alike: the
/// live one links its notices as `/News/Notice/Inspection/150506`, the test world's as
/// `/testworld/news/inspect/195`.
pub fn read_board(html: &str) -> Vec<Notice> {
    const LINKS: [&str; 2] = ["href=\"/news/notice/inspection/", "href=\"/testworld/news/inspect/"];
    // Links to notices also occur in the page's menus; the list itself comes after this.
    let list = html.find("class=\"news_board\"").map_or("", |at| &html[at..]);
    // The same text with its Latin letters in lower case, so that a byte is at the same place
    // in both: the two boards do not agree on how to write their own addresses.
    let lower = list.to_ascii_lowercase();
    let mut notices = Vec::new();
    let mut from = 0;
    while let Some((at, link)) =
        LINKS.iter().filter_map(|link| Some((from + lower[from..].find(link)?, link))).min_by_key(|(at, _)| *at)
    {
        from = at + link.len();
        let rest = &list[from..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let Ok(id) = rest[..digits].parse() else { continue };
        // The title is the text of the first <span> inside the link.
        let link = &rest[..rest.find("</a>").unwrap_or(rest.len())];
        let title = link.find("<span>").and_then(|open| {
            let text = &link[open + "<span>".len()..];
            Some(plain(&text[..text.find("</span>")?]))
        });
        if let Some(title) = title.filter(|title| !title.is_empty()) {
            notices.push(Notice { id, title });
        }
    }
    notices
}

/// Whether a page of the test world says that the test world is closed altogether these
/// days: it is only open during its operating periods.
pub fn out_of_season(html: &str) -> bool {
    html.contains("운영기간이 아닙니다")
}

/// Text of a piece of HTML or XML: tags taken out (the test world's board marks an edited
/// notice with one inside the title), the few entities a title can carry, and spaces tidied.
fn plain(text: &str) -> String {
    let mut bare = String::with_capacity(text.len());
    let mut inside = false;
    for letter in text.chars() {
        match letter {
            '<' => inside = true,
            '>' if inside => {
                inside = false;
                bare.push(' ');
            }
            _ if !inside => bare.push(letter),
            _ => {}
        }
    }
    let text = bare.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Notice {
    /// Nexon has marked it as over: `[점검완료]`, `[패치완료]`, and on the test world's board
    /// also `[점검종료]`.
    pub fn finished(&self) -> bool {
        self.title.contains("완료]") || self.title.contains("종료]")
    }

    /// Nexon has marked it as extended: `(연장)`, `(연장2)`.
    pub fn extended(&self) -> bool {
        self.title.contains("(연장")
    }

    /// Whether nobody can play while it lasts: a patch or a check of all worlds, with hours.
    /// A check of one world or channel, of the web site or of the auction house is not, and
    /// neither is a patch that is applied while the game stays open (`16:05 적용`).
    pub fn closes_game(&self) -> bool {
        let title = &self.title;
        let every_world = title.contains("전체 월드") || title.contains("전체 테스트월드");
        let whole_game = every_world || (title.contains("패치") && title.contains("ver"));
        whole_game && hours(title).is_some() && day(title).is_some()
    }

    /// When it begins and when it is planned to end, read from the title. `now` settles the
    /// year, which the title does not state.
    pub fn window(&self, now: Minutes) -> Option<(Minutes, Minutes)> {
        let (month, day) = day(&self.title)?;
        let (from, to) = hours(&self.title)?;
        let (year, ..) = civil_from_days(now.div_euclid(1440));
        // The year that puts the day nearest to now: a notice read on January 1 may be about
        // December 31.
        let begins = (year - 1..=year + 1)
            .map(|year| days_from_civil(year, month, day) * 1440 + from)
            .min_by_key(|begins| (begins - now).abs())?;
        // Hours that end before they begin run past midnight.
        let length = if to > from { to - from } else { to + 1440 - from };
        Some((begins, begins + length))
    }
}

/// The day in a title: `11/19(목)` is November 19.
fn day(title: &str) -> Option<(u32, u32)> {
    let bytes = title.as_bytes();
    for (slash, _) in title.match_indices('/') {
        let before = bytes[..slash].iter().rev().take_while(|b| b.is_ascii_digit()).count();
        let after = bytes[slash + 1..].iter().take_while(|b| b.is_ascii_digit()).count();
        if !(1..=2).contains(&before) || !(1..=2).contains(&after) {
            continue;
        }
        let month: u32 = title[slash - before..slash].parse().ok()?;
        let day: u32 = title[slash + 1..slash + 1 + after].parse().ok()?;
        // The weekday follows in brackets; a date without one is something else (a version,
        // a fraction).
        let weekday = bytes.get(slash + 1 + after) == Some(&b'(');
        if weekday && (1..=12).contains(&month) && (1..=31).contains(&day) {
            return Some((month, day));
        }
    }
    None
}

/// The hours in a title, as minutes of the day: `(4:00~10:00)` is 240 and 600. The last such
/// pair counts.
fn hours(title: &str) -> Option<(Minutes, Minutes)> {
    let clock = |text: &str| -> Option<Minutes> {
        let (hour, minute) = text.trim().split_once(':')?;
        let (hour, minute): (Minutes, Minutes) = (hour.trim().parse().ok()?, minute.trim().parse().ok()?);
        ((0..=24).contains(&hour) && (0..60).contains(&minute)).then_some(hour * 60 + minute)
    };
    title
        .split('(')
        .filter_map(|part| {
            let (from, to) = part[..part.find(')')?].split_once('~')?;
            Some((clock(from)?, clock(to)?))
        })
        .next_back()
}

/// Days since 1970 of a date. (Howard Hinnant's `days_from_civil`.)
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month = i64::from(month);
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The date of a number of days since 1970.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 } as u32;
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

/// A maintenance whose notice is still not marked as over this long after its planned end
/// has been forgotten on the board rather than extended: an extension is written down.
const FORGOTTEN_AFTER: Minutes = 6 * 60;

/// One maintenance, put together from what the service and the board say about it.
struct Span {
    begins: Minutes,
    ends: Minutes,
    title: String,
    /// The board has a notice of it.
    noticed: bool,
    /// That notice is marked as over.
    over: bool,
    /// That notice is marked as extended.
    extended: bool,
}

impl Span {
    /// Whether the game is closed for it at `now`.
    ///
    /// The planned end is not the end: maintenance runs over, and is extended at the last
    /// minute. One the board has a notice of lasts until that notice says it is over. Only
    /// one the board says nothing of goes by the clock.
    fn underway(&self, now: Minutes) -> bool {
        !self.over && now >= self.begins && (now < self.ends || (self.noticed && now <= self.ends + FORGOTTEN_AFTER))
    }

    /// Whether this is the maintenance that began at `begins`.
    fn began_at(&self, begins: Minutes) -> bool {
        self.begins <= begins && begins < self.ends.max(self.begins + 1)
    }
}

/// Every maintenance the two sources know of.
fn spans(planned: &[Planned], notices: &[Notice], now: Minutes) -> Vec<Span> {
    let mut spans: Vec<Span> = planned
        .iter()
        .map(|planned| Span {
            begins: planned.begins,
            ends: planned.ends,
            title: planned.text.clone(),
            noticed: false,
            over: false,
            extended: false,
        })
        .collect();
    for notice in notices.iter().filter(|notice| notice.closes_game()) {
        let Some((begins, ends)) = notice.window(now) else { continue };
        match spans.iter_mut().find(|span| span.begins < ends && begins < span.ends) {
            // Two accounts of one maintenance. An extension may reach either of them first,
            // so the later end is the one that holds.
            Some(span) => {
                span.ends = span.ends.max(ends);
                span.noticed = true;
                span.over |= notice.finished();
                span.extended |= notice.extended();
            }
            None => spans.push(Span {
                begins,
                ends,
                title: notice.title.clone(),
                noticed: true,
                over: notice.finished(),
                extended: notice.extended(),
            }),
        }
    }
    spans
}

/// The maintenance the game is closed for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Underway {
    pub title: String,
    /// When it is planned to end.
    pub ends: Minutes,
    /// It has been extended: the board marks it so, or its planned end was seen to move.
    pub extended: bool,
}

/// What one look at the two sources says.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reading {
    /// The maintenance that was underway at the look before is over.
    pub ended: bool,
    /// The maintenance underway has been extended: its new planned end.
    pub extended: Option<Minutes>,
    /// The maintenance underway now.
    pub underway: Option<Underway>,
    /// A maintenance is underway: it can end early, be extended or run over at any moment, so
    /// the next look should come soon. Without one there is nothing to hurry for.
    pub soon: bool,
}

/// Follows the game's maintenance from one look to the next.
#[derive(Debug, Default)]
pub struct Watch {
    /// The maintenance seen underway at the last look: when it began, its planned end, and
    /// whether it was known to be extended.
    seen: Option<(Minutes, Minutes, bool)>,
}

impl Watch {
    /// Takes in what the service (`planned`) and the board (`notices`) say at `now`.
    pub fn read(&mut self, planned: &[Planned], notices: &[Notice], now: Minutes) -> Reading {
        let spans = spans(planned, notices, now);
        let current = spans.iter().filter(|span| span.underway(now)).max_by_key(|span| span.ends);
        let mut reading = Reading::default();
        // The one underway was extended at an earlier look, or is at this one.
        let mut moved = false;
        if let Some((begins, ends, extended)) = self.seen {
            match current.filter(|span| span.began_at(begins)) {
                Some(span) => {
                    reading.extended = (span.ends > ends).then_some(span.ends);
                    moved = extended || span.ends > ends;
                }
                // It is over when it is still listed and says so: marked as over on the
                // board, or, where the board has no notice of it, past its time. One that
                // has merely left both lists, or was forgotten on the board, is no news.
                None => reading.ended = spans.iter().any(|span| span.began_at(begins) && (span.over || !span.noticed)),
            }
        }
        self.seen = current.map(|span| (span.begins, span.ends, span.extended || moved));
        reading.underway = current.map(|span| Underway {
            title: span.title.clone(),
            ends: span.ends,
            extended: span.extended || moved,
        });
        reading.soon = current.is_some();
        reading
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(id: u32, title: &str) -> Notice {
        Notice { id, title: title.to_owned() }
    }

    /// Minutes since 1970 of a Korean date and time.
    fn at(year: i64, month: u32, day: u32, hour: i64, minute: i64) -> Minutes {
        days_from_civil(year, month, day) * 1440 + hour * 60 + minute
    }

    fn planned(begins: Minutes, ends: Minutes) -> Planned {
        Planned {
            begins, ends, text: "[패치중] 11/26(목) ver1.2.422 클라이언트 패치(04:00~10:00)".to_owned()
        }
    }

    fn underway(title: &str, ends: Minutes) -> Option<Underway> {
        Some(Underway { title: title.to_owned(), ends, extended: false })
    }

    // The list as the page has it, cut down to three notices of 2026-11-25.
    const PAGE: &str = r#"
        <li><a href="/News/Notice/Inspection">점검</a></li>
        <div class="news_board">
            <ul>
                <li><p>
                    <a href="/News/Notice/Inspection/150506">
                        <em><img src="https://ssl.nexon.com/s2/game/maplestory/renewal/common/notice_icon03.png" alt="[점검]" /></em>
                        <span>[패치완료] 11/24(화) ver1.2.421 마이너버전(4) 패치 (16:05 적용)</span>
                    </a>
                </p></li>
                <li><p>
                    <a href="/News/Notice/Inspection/150494">
                        <em><img src="notice_icon03.png" alt="[점검]" /></em>
                        <span>[점검완료] 11/19(목) 전체 월드 채널 점검 (17:00~17:50)</span>
                    </a>
                </p></li>
                <li><p>
                    <a href="/News/Notice/Inspection/150480">
                        <em><img src="notice_icon03.png" alt="[점검]" /></em>
                        <span>[점검완료] 11/19(목) ver1.2.421   클라이언트 패치(4:00~10:00) &amp; 안내</span>
                    </a>
                </p></li>
            </ul>
        </div>"#;

    // The service's answer of 2026-11-25, with the description of its own layout shortened.
    const ANSWER: &str = concat!(
        r#"<?xml version="1.0" encoding="utf-8"?><soap:Envelope><soap:Body><GetInspectionInfoResponse>"#,
        r#"<GetInspectionInfoResult><xs:schema id="NewDataSet"><xs:element name="InspectionInfo"><xs:complexType>"#,
        r#"<xs:sequence><xs:element name="startDateTime" type="xs:dateTime" minOccurs="0" /></xs:sequence>"#,
        r#"</xs:complexType></xs:element></xs:schema><diffgr:diffgram><NewDataSet xmlns="">"#,
        r#"<InspectionInfo diffgr:id="InspectionInfo1" msdata:rowOrder="0" diffgr:hasChanges="inserted">"#,
        r#"<serviceCode>2</serviceCode><startDateTime>2026-11-19T04:00:00+09:00</startDateTime>"#,
        r#"<endDateTime>2026-11-19T10:00:00+09:00</endDateTime>"#,
        r#"<strObstacleContents>[패치중] 11/19(목) ver1.2.421 클라이언트 패치(04:00~10:00)</strObstacleContents>"#,
        r#"</InspectionInfo></NewDataSet></diffgr:diffgram></GetInspectionInfoResult>"#,
        r#"</GetInspectionInfoResponse></soap:Body></soap:Envelope>"#
    );

    #[test]
    fn the_board_is_read_newest_first() {
        let notices = read_board(PAGE);
        assert_eq!(notices.len(), 3);
        assert_eq!(notices[0], notice(150506, "[패치완료] 11/24(화) ver1.2.421 마이너버전(4) 패치 (16:05 적용)"));
        assert_eq!(notices[1].id, 150494);
        assert_eq!(notices[2].title, "[점검완료] 11/19(목) ver1.2.421 클라이언트 패치(4:00~10:00) & 안내");
        assert!(read_board("<html>nothing of the kind</html>").is_empty());
    }

    #[test]
    fn the_service_states_the_hours_as_figures() {
        let listed = read_inspection(ANSWER);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].begins, at(2026, 11, 19, 4, 0));
        assert_eq!(listed[0].ends, at(2026, 11, 19, 10, 0));
        assert_eq!(listed[0].text, "[패치중] 11/19(목) ver1.2.421 클라이언트 패치(04:00~10:00)");
        assert!(read_inspection("<soap:Envelope><soap:Body /></soap:Envelope>").is_empty());

        // Stamps in another zone, in UTC, and with fractions of a second are Korean time all the same.
        assert_eq!(stamp("2026-11-18T19:00:00Z"), Some(at(2026, 11, 19, 4, 0)));
        assert_eq!(stamp("2026-11-18T14:00:00.000-05:00"), Some(at(2026, 11, 19, 4, 0)));
        assert_eq!(stamp("2026-11-19T04:00:00"), Some(at(2026, 11, 19, 4, 0)));
        assert_eq!(stamp("soon"), None);
        assert_eq!(clock(at(2026, 11, 19, 4, 5)), "04:05");
    }

    #[test]
    fn only_maintenance_that_closes_the_whole_game_counts() {
        // Titles as Nexon has written them in 2026.
        let closing = [
            "[패치완료] 10/22(목) ver1.2.420 클라이언트 패치(4:00~10:00)",
            "(연장2)[패치완료] 6/18(목) ver1.2.416클라이언트 패치(4:00~12:00)",
            "[패치완료] 7/24(금) ver1.2.417 마이너(3) 패치(07:00~09:00)",
            "(수정)(연장) [점검완료] 7/6(월) 전체 월드 채널 점검 (08:00~09:30)",
            "11/26(목) 전체 월드 채널 점검 (17:00~17:50)",
        ];
        for title in closing {
            assert!(notice(1, title).closes_game(), "{title}");
        }
        let open = [
            "[패치완료] 11/24(화) ver1.2.421 마이너버전(4) 패치 (16:05 적용)",
            "(수정)[점검완료] 11/19(목) 메이플스토리 홈페이지 점검 안내(03:00~10:00)",
            "[점검완료] 6/22(월) 메이플 옥션 서버점검 (14:00~15:00)",
            "[점검완료]8/17(월) 엘리시움 월드 20채널 점검 (12:35~12:45)",
            "[점검완료] 6/25(목) 챌린저스 1~4 월드 채널 점검 (11:50~12:50)",
            "(연장)[점검완료] 6/18(목) 챌린저스4 월드, 일부 컨텐츠 서버점검",
        ];
        for title in open {
            assert!(!notice(1, title).closes_game(), "{title}");
        }
    }

    #[test]
    fn the_day_and_the_hours_come_from_the_title() {
        let now = at(2026, 11, 25, 20, 0);
        let patch = notice(1, "(연장2)[패치완료] 6/18(목) ver1.2.416클라이언트 패치(4:00~12:00)");
        assert_eq!(patch.window(now), Some((at(2026, 6, 18, 4, 0), at(2026, 6, 18, 12, 0))));
        assert!(patch.finished());

        let short = notice(2, "[점검완료] 6/18(목) 메이플 옥션 서버점검 (0:30~0:43)");
        assert_eq!(short.window(now), Some((at(2026, 6, 18, 0, 30), at(2026, 6, 18, 0, 43))));

        // Past midnight, and across the turn of the year in both directions.
        let night = notice(3, "12/31(목) 전체 월드 채널 점검 (23:00~01:00)");
        assert!(!night.finished());
        assert_eq!(night.window(at(2027, 1, 1, 0, 30)), Some((at(2026, 12, 31, 23, 0), at(2027, 1, 1, 1, 0))));
        let ahead = notice(4, "1/2(토) 전체 월드 채널 점검 (4:00~10:00)");
        assert_eq!(ahead.window(at(2026, 12, 30, 9, 0)), Some((at(2027, 1, 2, 4, 0), at(2027, 1, 2, 10, 0))));

        assert_eq!(notice(5, "[패치완료] 11/24(화) ver1.2.421 마이너버전(4) 패치 (16:05 적용)").window(now), None);
        assert_eq!(notice(6, "ver1.2/3 패치 안내").window(now), None);
    }

    #[test]
    fn the_calendar_runs_both_ways() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        for (year, month, day) in [(2026, 11, 25), (2024, 2, 29), (2027, 1, 1), (2026, 12, 31), (2000, 3, 1)] {
            assert_eq!(civil_from_days(days_from_civil(year, month, day)), (year, month, day));
        }
        // 2026-11-25 20:00 in Korea is 11:00 UTC that day.
        assert_eq!(at(2026, 11, 25, 20, 0) * 60 - 9 * 3600, 1_795_604_400);
    }

    // The test world's list as its page has it, cut down to three notices, and the panel at
    // the side that says it is closed between operating periods.
    const TEST_PAGE: &str = r#"
        <li><a href="/Testworld/News/Inspect">점검</a></li>
        <div class="news_board">
            <ul>
                <li><p>
                    <a href="/testworld/news/inspect/195">
                        <em><img src="notice_icon03.png" alt="" /></em>
                        <span><em class="modify_common" data-modifytime="2026-08-14 15:30">수정</em> [점검종료] 8/14(금) 전체 테스트월드 채널 점검 (15:50~16:10)</span>
                    </a>
                </p></li>
                <li><p>
                    <a href="/testworld/news/inspect/190">
                        <span> [패치완료] 7/16(목) ver1.2.204 마이너(2) 패치 (17:34 적용)</span>
                    </a>
                </p></li>
                <li><p>
                    <a href="/testworld/news/inspect/160">
                        <span> 11/18(화) ver1.2.195 마이너(4) 패치 (10:00~11:00)</span>
                    </a>
                </p></li>
            </ul>
        </div>
        <div class="test_wrold_side"><h1>테스트월드 운영기간이 아닙니다.</h1></div>
    "#;

    #[test]
    fn the_test_worlds_board_is_read_like_the_live_one() {
        let notices = read_board(TEST_PAGE);
        assert_eq!(notices.iter().map(|notice| notice.id).collect::<Vec<_>>(), [195, 190, 160]);
        // The mark of an edited notice is a tag inside the title; its word stays, the tag goes.
        assert_eq!(notices[0].title, "수정 [점검종료] 8/14(금) 전체 테스트월드 채널 점검 (15:50~16:10)");
        assert!(notices[0].closes_game() && notices[0].finished());
        // Applied while the test world stays open.
        assert!(!notices[1].closes_game());
        // A patch with hours closes it, and this one is not marked as over.
        assert!(notices[2].closes_game() && !notices[2].finished());
        assert_eq!(notices[2].window(at(2025, 11, 18, 9, 0)), Some((at(2025, 11, 18, 10, 0), at(2025, 11, 18, 11, 0))));

        assert!(out_of_season(TEST_PAGE));
        assert!(!out_of_season(PAGE));
    }

    #[test]
    fn each_client_has_its_own_sources() {
        assert!(LIVE.board_url.ends_with("/News/Notice/Inspection"));
        let service = LIVE.inspection.as_ref().expect("the service lists the live game");
        assert!(service.url.starts_with("https://") && service.request.contains("GetInspectionInfo"));
        assert!(TEST.board_url.ends_with("/Testworld/News/Inspect"));
        assert!(TEST.inspection.is_none());
    }

    const PATCH: &str = "11/26(목) ver1.2.422 클라이언트 패치(4:00~10:00)";
    const PATCH_OVER: &str = "[패치완료] 11/26(목) ver1.2.422 클라이언트 패치(4:00~10:00)";

    #[test]
    fn a_maintenance_lasts_until_the_board_says_it_is_over() {
        let listed = [planned(at(2026, 11, 26, 4, 0), at(2026, 11, 26, 10, 0))];
        let other = notice(8, "[패치완료] 11/24(화) ver1.2.421 마이너버전(4) 패치 (16:05 적용)");
        let (open, over) = (notice(9, PATCH), notice(9, PATCH_OVER));
        let mut watch = Watch::default();

        // The evening before: announced, nothing to do yet.
        assert_eq!(watch.read(&listed, &[open.clone(), other.clone()], at(2026, 11, 25, 20, 0)), Reading::default());
        // Minutes before it begins there is still nothing to hurry for.
        assert_eq!(watch.read(&listed, std::slice::from_ref(&open), at(2026, 11, 26, 3, 55)), Reading::default());
        // Underway, and still underway when the planned end has passed without a word. All
        // that time the next look should come soon.
        for now in [at(2026, 11, 26, 4, 0), at(2026, 11, 26, 9, 59), at(2026, 11, 26, 10, 20)] {
            let reading = watch.read(&listed, &[open.clone(), other.clone()], now);
            let closed = underway(&listed[0].text, at(2026, 11, 26, 10, 0));
            assert_eq!(reading, Reading { underway: closed, soon: true, ..Reading::default() });
        }
        // Marked as over: it ends, once.
        let reading = watch.read(&listed, &[over.clone(), other.clone()], at(2026, 11, 26, 10, 21));
        assert_eq!(reading, Reading { ended: true, ..Reading::default() });
        assert_eq!(watch.read(&listed, &[over, other], at(2026, 11, 26, 10, 22)), Reading::default());
    }

    #[test]
    fn a_maintenance_that_is_over_early_ends_when_the_board_says_so() {
        let listed = [planned(at(2026, 11, 26, 4, 0), at(2026, 11, 26, 10, 0))];
        let mut watch = Watch::default();
        assert!(watch.read(&listed, &[notice(9, PATCH)], at(2026, 11, 26, 8, 0)).underway.is_some());
        let reading = watch.read(&listed, &[notice(9, PATCH_OVER)], at(2026, 11, 26, 9, 10));
        assert_eq!(reading, Reading { ended: true, ..Reading::default() });
    }

    #[test]
    fn an_extension_is_heard_from_whichever_source_has_it_first() {
        let listed = [planned(at(2026, 11, 26, 4, 0), at(2026, 11, 26, 10, 0))];
        let mut watch = Watch::default();
        assert_eq!(watch.read(&listed, &[notice(9, PATCH)], at(2026, 11, 26, 9, 30)).extended, None);

        // The board first: the title gets the mark and the new hours.
        let longer = notice(9, "(연장)11/26(목) ver1.2.422 클라이언트 패치(4:00~12:00)");
        let reading = watch.read(&listed, std::slice::from_ref(&longer), at(2026, 11, 26, 9, 40));
        assert_eq!(reading.extended, Some(at(2026, 11, 26, 12, 0)));
        assert_eq!(
            reading.underway.map(|closed| (closed.ends, closed.extended)),
            Some((at(2026, 11, 26, 12, 0), true))
        );
        // Heard once; the service catching up changes nothing.
        let caught_up = [planned(at(2026, 11, 26, 4, 0), at(2026, 11, 26, 12, 0))];
        assert_eq!(watch.read(&caught_up, std::slice::from_ref(&longer), at(2026, 11, 26, 9, 41)).extended, None);

        // The service first: a second extension.
        let again = [planned(at(2026, 11, 26, 4, 0), at(2026, 11, 26, 13, 30))];
        let reading = watch.read(&again, &[longer], at(2026, 11, 26, 11, 50));
        assert_eq!(reading.extended, Some(at(2026, 11, 26, 13, 30)));
        assert!(!reading.ended);

        // Over, with the hours as they ended up.
        let over = notice(9, "(연장2)[패치완료] 11/26(목) ver1.2.422 클라이언트 패치(4:00~13:30)");
        assert_eq!(
            watch.read(&again, &[over], at(2026, 11, 26, 13, 20)),
            Reading { ended: true, ..Reading::default() }
        );
    }

    #[test]
    fn an_extended_maintenance_is_known_as_one_for_as_long_as_it_lasts() {
        // The service alone moves the end: no mark anywhere, but the end was seen to move, and
        // that is remembered at the looks that follow.
        let listed = [planned(at(2026, 11, 26, 4, 0), at(2026, 11, 26, 10, 0))];
        let longer = [planned(at(2026, 11, 26, 4, 0), at(2026, 11, 26, 12, 0))];
        let mut watch = Watch::default();
        let extended = |reading: Reading| reading.underway.map(|closed| closed.extended);
        assert_eq!(extended(watch.read(&listed, &[], at(2026, 11, 26, 9, 0))), Some(false));
        assert_eq!(extended(watch.read(&longer, &[], at(2026, 11, 26, 9, 50))), Some(true));
        assert_eq!(extended(watch.read(&longer, &[], at(2026, 11, 26, 11, 0))), Some(true));

        // An app started after the extension knows of it from the mark on the board.
        let marked = notice(9, "(연장)11/26(목) ver1.2.422 클라이언트 패치(4:00~12:00)");
        let mut late = Watch::default();
        assert_eq!(extended(late.read(&longer, &[marked], at(2026, 11, 26, 11, 0))), Some(true));

        // The next maintenance starts as an ordinary one.
        let next = [planned(at(2026, 12, 3, 4, 0), at(2026, 12, 3, 10, 0))];
        assert_eq!(extended(watch.read(&next, &[], at(2026, 12, 3, 5, 0))), Some(false));
    }

    #[test]
    fn without_a_notice_a_maintenance_goes_by_the_clock() {
        // The board could not be read, or has no notice of what the service lists.
        let listed = [planned(at(2026, 11, 26, 4, 0), at(2026, 11, 26, 10, 0))];
        let mut watch = Watch::default();
        assert!(watch.read(&listed, &[], at(2026, 11, 26, 9, 59)).underway.is_some());
        assert_eq!(watch.read(&listed, &[], at(2026, 11, 26, 10, 0)), Reading { ended: true, ..Reading::default() });
    }

    #[test]
    fn a_maintenance_only_the_board_knows_of_counts_too() {
        // The short checks of all worlds are not in the service's list.
        let old = [planned(at(2026, 11, 19, 4, 0), at(2026, 11, 19, 10, 0))];
        let check = "11/26(목) 전체 월드 채널 점검 (17:00~17:50)";
        let mut watch = Watch::default();
        let reading = watch.read(&old, &[notice(7, check)], at(2026, 11, 26, 17, 55));
        assert_eq!(reading.underway, underway(check, at(2026, 11, 26, 17, 50)));
        let over = notice(7, "[점검완료] 11/26(목) 전체 월드 채널 점검 (17:00~17:50)");
        assert_eq!(watch.read(&old, &[over], at(2026, 11, 26, 17, 58)), Reading { ended: true, ..Reading::default() });
    }

    #[test]
    fn what_was_over_before_the_watch_began_is_no_news() {
        let listed = [planned(at(2026, 11, 26, 4, 0), at(2026, 11, 26, 10, 0))];
        let mut watch = Watch::default();
        assert_eq!(watch.read(&listed, &[notice(9, PATCH_OVER)], at(2026, 11, 26, 10, 5)), Reading::default());
        assert_eq!(watch.read(&listed, &[], at(2026, 11, 27, 9, 0)), Reading::default());
    }

    #[test]
    fn a_forgotten_notice_and_one_that_is_taken_down_are_no_news() {
        // Never marked as over: a day later it is not taken for a maintenance any more, and
        // marking it then is no news.
        let stale = notice(7, "11/20(금) 전체 월드 채널 점검 (17:00~17:50)");
        let mut watch = Watch::default();
        assert!(watch.read(&[], std::slice::from_ref(&stale), at(2026, 11, 20, 17, 10)).underway.is_some());
        assert_eq!(watch.read(&[], &[stale], at(2026, 11, 21, 17, 10)), Reading::default());
        let marked = notice(7, "[점검완료] 11/20(금) 전체 월드 채널 점검 (17:00~17:50)");
        assert_eq!(watch.read(&[], &[marked], at(2026, 11, 21, 17, 11)), Reading::default());

        // Taken off the board while underway.
        let mut watch = Watch::default();
        let open = notice(9, "11/26(목) 전체 월드 채널 점검 (17:00~17:50)");
        assert!(watch.read(&[], &[open], at(2026, 11, 26, 17, 5)).underway.is_some());
        assert_eq!(watch.read(&[], &[], at(2026, 11, 26, 17, 6)), Reading::default());
    }
}
