//! What the made-up world says about maintenance: the answer of the inspection service and
//! the two notice boards, as files that the made-up server hands out and a tester may edit.
//!
//! The wording is Nexon's, as `mfp_kms::notice` reads it. The live game's maintenance is a
//! client patch that the inspection service lists and the board announces; an extension is
//! written in front of the title with the new hours, and `[패치완료]` when it is over. The
//! test world's is a check of all its channels, on its own board only, and over with
//! `[점검종료]`.

use std::fs;
use std::path::Path;

use mfp_kms::Server;
use mfp_kms::notice::{Minutes, clock};

use crate::calendar::{notice_day, service_stamp};

/// The files the made-up server answers from, in the folder of a made-up world.
pub const INSPECTION_FILE: &str = "inspection.xml";
pub const BOARD_FILE: &str = "board.html";
pub const TEST_BOARD_FILE: &str = "board-test.html";

/// How long a maintenance is announced for, and by how much one extension moves its end.
const HOUR: Minutes = 60;

/// What a tester does to the maintenance of one client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Announces one that begins in so many minutes and lasts an hour.
    Announce { minutes: Minutes },
    /// Announces one as underway from now for the next hour.
    Begin,
    /// Moves the end of the current one an hour on, marked as an extension.
    Extend,
    /// Marks the current one as over.
    End,
}

/// The maintenance the made-up world is telling of, for one client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Maintenance {
    /// The number of its notice on the board.
    pub id: u32,
    pub begins: Minutes,
    pub ends: Minutes,
    /// How often it has been extended.
    pub extensions: u32,
    pub over: bool,
    /// The version the live game's title names: `1.2.902`.
    pub label: String,
}

impl Maintenance {
    /// The title of its notice on the board of `server`.
    pub fn title(&self, server: Server) -> String {
        let extended = match self.extensions {
            0 => String::new(),
            1 => "(연장)".to_owned(),
            more => format!("(연장{more})"),
        };
        let (day, from, to) = (notice_day(self.begins), clock(self.begins), clock(self.ends));
        match server {
            Server::Live => {
                let over = if self.over { "[패치완료] " } else { "" };
                // The live board writes the hours without a zero in front.
                let short = |clock: &str| clock.strip_prefix('0').unwrap_or(clock).to_owned();
                format!("{extended}{over}{day} ver{} 클라이언트 패치({}~{})", self.label, short(&from), short(&to))
            }
            Server::Test => {
                let over = if self.over { "[점검종료] " } else { "" };
                format!("{extended}{over}{day} 전체 테스트월드 채널 점검 ({from}~{to})")
            }
        }
    }

    /// The line the inspection service shows for it.
    fn service_text(&self) -> String {
        let (day, from, to) = (notice_day(self.begins), clock(self.begins), clock(self.ends));
        format!("[패치중] {day} ver{} 클라이언트 패치({from}~{to})", self.label)
    }

    fn state_file(server: Server) -> String {
        format!("maintenance-{}.txt", server.key())
    }

    /// What was last told of the maintenance of `server` in the world at `dir`.
    pub fn read(dir: &Path, server: Server) -> Option<Maintenance> {
        let text = fs::read_to_string(dir.join(Maintenance::state_file(server))).ok()?;
        let mut parts = text.split_whitespace();
        Some(Maintenance {
            id: parts.next()?.parse().ok()?,
            begins: parts.next()?.parse().ok()?,
            ends: parts.next()?.parse().ok()?,
            extensions: parts.next()?.parse().ok()?,
            over: parts.next()? == "over",
            label: parts.next()?.to_owned(),
        })
    }

    fn write(&self, dir: &Path, server: Server) -> Result<(), String> {
        let Maintenance { id, begins, ends, extensions, over, label } = self;
        let text = format!("{id} {begins} {ends} {extensions} {} {label}\n", if *over { "over" } else { "open" });
        fs::write(dir.join(Maintenance::state_file(server)), text).map_err(|e| format!("write the maintenance: {e}"))
    }
}

/// The number the first notice of a made-up maintenance gets on a board.
fn first_id(server: Server) -> u32 {
    match server {
        Server::Live => 150_507,
        Server::Test => 196,
    }
}

/// Changes what the world at `dir` tells of the maintenance of `server` and writes the
/// three files again. `label` is the version a new maintenance of the live game is for.
pub fn change(dir: &Path, server: Server, change: Change, label: &str, now: Minutes) -> Result<Maintenance, String> {
    let current = Maintenance::read(dir, server);
    let fresh = |begins: Minutes| Maintenance {
        id: current.as_ref().map_or(first_id(server), |last| last.id + 1),
        begins,
        ends: begins + HOUR,
        extensions: 0,
        over: false,
        label: label.to_owned(),
    };
    let open = || current.clone().filter(|last| !last.over).ok_or("no maintenance has been announced or begun");
    let next = match change {
        Change::Announce { minutes } => fresh(now + minutes),
        Change::Begin => fresh(now),
        Change::Extend => {
            let last = open()?;
            Maintenance { ends: last.ends + HOUR, extensions: last.extensions + 1, ..last }
        }
        Change::End => Maintenance { over: true, ..open()? },
    };
    next.write(dir, server)?;
    write_files(dir, now)?;
    Ok(next)
}

/// Writes the three files of the world at `dir` from what was last told of each client's
/// maintenance. Without any, they tell of nothing but maintenance that is long over.
pub fn write_files(dir: &Path, now: Minutes) -> Result<(), String> {
    let (live, test) = (Maintenance::read(dir, Server::Live), Maintenance::read(dir, Server::Test));
    let files = [
        (INSPECTION_FILE, inspection(live.as_ref(), now)),
        (BOARD_FILE, board(Server::Live, live.as_ref(), now)),
        (TEST_BOARD_FILE, board(Server::Test, test.as_ref(), now)),
    ];
    for (name, text) in files {
        fs::write(dir.join(name), text).map_err(|e| format!("write {name}: {e}"))?;
    }
    Ok(())
}

/// A maintenance of the live game that was over a week before `now`: what the service and
/// the board still show when nothing is going on.
fn last_week(now: Minutes) -> Maintenance {
    let begins = (now - 7 * 1440).div_euclid(1440) * 1440 + 4 * 60;
    Maintenance { id: 150_480, begins, ends: begins + 6 * HOUR, extensions: 0, over: true, label: "1.2.421".to_owned() }
}

/// The answer of the inspection service. It lists the latest maintenance of the live game,
/// also long after it is over, and describes its own layout first.
fn inspection(current: Option<&Maintenance>, now: Minutes) -> String {
    let old = last_week(now);
    let listed = current.unwrap_or(&old);
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="utf-8"?>"#,
            r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body>"#,
            r#"<GetInspectionInfoResponse xmlns="https://api.maplestory.nexon.com/soap/"><GetInspectionInfoResult>"#,
            r#"<xs:schema id="NewDataSet" xmlns:xs="http://www.w3.org/2001/XMLSchema">"#,
            r#"<xs:element name="InspectionInfo"><xs:complexType><xs:sequence>"#,
            r#"<xs:element name="startDateTime" type="xs:dateTime" minOccurs="0" />"#,
            r#"<xs:element name="endDateTime" type="xs:dateTime" minOccurs="0" />"#,
            r#"</xs:sequence></xs:complexType></xs:element></xs:schema>"#,
            r#"<diffgr:diffgram xmlns:diffgr="urn:schemas-microsoft-com:xml-diffgram-v1"><NewDataSet xmlns="">"#,
            r#"<InspectionInfo diffgr:id="InspectionInfo1" diffgr:hasChanges="inserted">"#,
            "<serviceCode>2</serviceCode><startDateTime>{}</startDateTime><endDateTime>{}</endDateTime>",
            "<strObstacleContents>{}</strObstacleContents>",
            "</InspectionInfo></NewDataSet></diffgr:diffgram>",
            "</GetInspectionInfoResult></GetInspectionInfoResponse></soap:Body></soap:Envelope>\n"
        ),
        service_stamp(listed.begins),
        service_stamp(listed.ends),
        listed.service_text()
    )
}

/// The first page of a notice board: the current maintenance on top, where there is one,
/// and below it two notices of a week and of two weeks ago that are long over.
fn board(server: Server, current: Option<&Maintenance>, now: Minutes) -> String {
    let (menu, link) = match server {
        Server::Live => ("/News/Notice/Inspection", "/News/Notice/Inspection"),
        Server::Test => ("/Testworld/News/Inspect", "/testworld/news/inspect"),
    };
    let old = last_week(now);
    let (week, fortnight) = (notice_day(old.begins), notice_day(old.begins - 7 * 1440));
    let mut notices: Vec<(u32, String)> = current.iter().map(|current| (current.id, current.title(server))).collect();
    notices.extend(match server {
        Server::Live => {
            [(150_494, format!("[점검완료] {week} 전체 월드 채널 점검 (17:00~17:50)")), (old.id, old.title(server))]
        }
        Server::Test => [
            (195, format!("[점검종료] {week} 전체 테스트월드 채널 점검 (15:50~16:10)")),
            (190, format!("[패치완료] {fortnight} ver1.2.204 마이너(2) 패치 (17:34 적용)")),
        ],
    });

    let mut page = String::from("<!-- Made up by fake_nexon. The server reads this file anew for every request. -->\n");
    page.push_str(&format!("<html><body>\n<ul><li><a href=\"{menu}\">점검</a></li></ul>\n"));
    page.push_str("<div class=\"news_board\">\n    <ul>\n");
    for (id, title) in notices {
        page.push_str(&format!(
            "        <li><p>\n            <a href=\"{link}/{id}\">\n                \
             <em><img src=\"notice_icon03.png\" alt=\"[점검]\" /></em>\n                \
             <span>{title}</span>\n            </a>\n        </p></li>\n"
        ));
    }
    page.push_str("    </ul>\n</div>\n</body></html>\n");
    page
}
