//! Value formatting for the UI: sizes, CPU, paths, ages and fixed-width cells.
//!
//! The words in these values ("about", "running for", the units of an age)
//! come from [`crate::i18n`]; this module only decides which one to use and
//! how to lay the number out. Everything here is total: no input panics.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::i18n::{Age, Lang, Text, age_text, tr};

/// What a value that cannot be read shows.
pub const NA: &str = "—";

/// A size in KB, MB or GB (units of 1024). Under 1 MB it is whole KB, under
/// 1 GB whole MB, above that GB with one decimal. Values are rounded, and the
/// unit is chosen after rounding so that "1024 KB" never shows up.
pub fn fmt_bytes(b: u64) -> String {
    const KIB: u128 = 1 << 10;
    const MIB: u128 = 1 << 20;
    const GIB: u128 = 1 << 30;
    let b = u128::from(b);
    let kb = (b + KIB / 2) / KIB;
    if kb < 1024 {
        return format!("{kb} KB");
    }
    let mb = (b + MIB / 2) / MIB;
    if mb < 1024 {
        return format!("{mb} MB");
    }
    let tenths = (b * 10 + GIB / 2) / GIB;
    format!("{}.{} GB", tenths / 10, tenths % 10)
}

/// A memory figure; `—` when unknown, marked as approximate when some of the
/// parts that went into it could not be read.
pub fn fmt_mem(b: Option<u64>, approx: bool, lang: Lang) -> String {
    match b {
        None => NA.to_string(),
        Some(b) if approx => format!("{}{}", tr(lang, Text::ApproxPrefix), fmt_bytes(b)),
        Some(b) => fmt_bytes(b),
    }
}

/// A CPU percentage: one decimal below 10, whole numbers from 10 up (so
/// 9.96 is "10%", never "10.0%"). `—` for unknown or non-finite values.
pub fn fmt_cpu(c: Option<f32>) -> String {
    let Some(v) = c.filter(|v| v.is_finite()) else {
        return NA.to_string();
    };
    let v = if v > 0.0 { v } else { 0.0 };
    if (v * 10.0).round() < 100.0 {
        format!("{v:.1}%")
    } else {
        format!("{v:.0}%")
    }
}

fn is_sep(c: char) -> bool {
    c == '/' || c == '\\'
}

/// A Windows path: starts with a drive letter and a separator (`C:\`), or is
/// a UNC path (`\\server\share`). A backslash alone does not count, since it
/// is a legal character in a Unix file name.
fn is_windows_path(s: &str) -> bool {
    let b = s.as_bytes();
    let drive = b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b.len() == 2 || b[2] == b'\\' || b[2] == b'/');
    drive || s.starts_with("\\\\")
}

/// The rest of `path` after `home`, when `home` is a whole leading part of it
/// (the next character must be a separator or the end, so `/Users/u2` does
/// not start with `/Users/u`). With `windows`, letters match regardless of
/// case and `/` equals `\`.
fn strip_home<'a>(path: &'a str, home: &str, windows: bool) -> Option<&'a str> {
    let mut rest = path.char_indices();
    for h in home.chars() {
        let (_, c) = rest.next()?;
        let same = if windows {
            c == h || (is_sep(c) && is_sep(h)) || c.to_lowercase().eq(h.to_lowercase())
        } else {
            c == h
        };
        if !same {
            return None;
        }
    }
    let rest = rest.as_str();
    match rest.chars().next() {
        None => Some(rest),
        Some(c) if c == '/' || (windows && c == '\\') => Some(rest),
        Some(_) => None,
    }
}

/// Replaces a leading `home` with `~`. Windows homes (drive letter or
/// backslashes) match case-insensitively and with either separator; other
/// paths are compared exactly. Needs a path boundary after the home.
pub fn shorten_home(path: &str, home: Option<&str>) -> String {
    let Some(home) = home else {
        return path.to_string();
    };
    let windows = is_windows_path(home);
    let home = home.trim_end_matches(|c| c == '/' || (windows && c == '\\'));
    if home.is_empty() {
        // "/" (or nothing) as the home would turn every path into "~/...".
        return path.to_string();
    }
    match strip_home(path, home, windows) {
        Some(rest) => format!("~{rest}"),
        None => path.to_string(),
    }
}

/// How long a process has been running, in minutes, hours or days (no
/// absolute time, so there is no time zone to get wrong). `—` when the start
/// time is unknown (0); a start time in the future counts as just started.
pub fn fmt_age(started: u64, now: u64, lang: Lang) -> String {
    if started == 0 {
        return NA.to_string();
    }
    let secs = now.saturating_sub(started);
    age_text(
        lang,
        match secs {
            0..=59 => Age::UnderMinute,
            60..=3599 => Age::Minutes(secs / 60),
            3600..=86_399 => Age::Hours(secs / 3600),
            _ => Age::Days(secs / 86_400),
        },
    )
}

/// `s` as one cell exactly `width` columns wide: too long and it is cut with
/// a trailing `…`, too short and it is padded with spaces.
///
/// Widths come from `unicode-width` (CJK takes 2 columns, combining marks 0),
/// and a combining mark always stays with its base character. Control
/// characters (tabs, newlines, escapes) become a space, so one value can
/// never break the line it is drawn on. When a wide character does not fit
/// in the column left for the text, the spare column becomes a space after
/// the `…` (so `fit_width("中文路", 4)` is `"中… "`).
pub fn fit_width(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let w = flat.width();
    if w <= width {
        return padded(flat, width);
    }
    // Keep as much of the start as leaves one column for the "…". The scan
    // stops at the first character that does not fit, so the zero-width
    // marks after it (which belong to it) are not kept without it.
    let budget = width - 1;
    let (mut used, mut cut) = (0, 0);
    for (i, c) in flat.char_indices() {
        let cw = c.width().unwrap_or(0);
        if used + cw > budget {
            break;
        }
        used += cw;
        cut = i + c.len_utf8();
    }
    let mut out = flat;
    out.truncate(cut);
    // Summing character widths can undercount what `str::width` reports for
    // some sequences (a heart plus U+FE0F is 2 columns): shrink until it fits.
    while out.width() >= width {
        if out.pop().is_none() {
            break;
        }
    }
    out.push('…');
    padded(out, width)
}

fn padded(mut s: String, width: usize) -> String {
    let w = s.width();
    s.extend(std::iter::repeat_n(' ', width.saturating_sub(w)));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    #[test]
    fn formatting() {
        assert_eq!(fmt_bytes(512 * 1024), "512 KB");
        assert_eq!(fmt_bytes(97 * 1024 * 1024), "97 MB");
        assert_eq!(fmt_bytes(1_900 * 1024 * 1024), "1.9 GB");
        assert_eq!(fmt_mem(None, false, Lang::ZhTw), "—");
        assert_eq!(
            fmt_mem(Some(1_900 * 1024 * 1024), true, Lang::ZhTw),
            "約 1.9 GB"
        );
        assert_eq!(
            fmt_mem(Some(1_900 * 1024 * 1024), true, Lang::En),
            "~1.9 GB"
        );
        assert_eq!(fmt_cpu(Some(0.4)), "0.4%");
        assert_eq!(fmt_cpu(Some(123.4)), "123%");
        assert_eq!(fmt_cpu(None), "—");
        assert_eq!(shorten_home("/Users/u/prj/x", Some("/Users/u")), "~/prj/x");
        assert_eq!(
            shorten_home("C:\\Users\\u\\prj", Some("C:\\Users\\u")),
            "~\\prj"
        );
        assert_eq!(fmt_age(1000, 1900, Lang::ZhTw), "已執行 15 分鐘");
        assert_eq!(fmt_age(1000, 1900, Lang::En), "running 15 min");
    }

    #[test]
    fn fit_width_respects_cjk() {
        let s = fit_width("中文路徑很長的專案名稱", 10);
        assert_eq!(UnicodeWidthStr::width(s.as_str()), 10);
        assert!(s.trim_end().ends_with('…')); // 中文字寬 2，湊不滿時尾端補空白
        assert_eq!(fit_width("ab", 4), "ab  ");
    }

    #[test]
    fn bytes_units_and_rollover() {
        assert_eq!(fmt_bytes(0), "0 KB");
        assert_eq!(fmt_bytes(1023 * KB), "1023 KB");
        // Rounds up into the next unit instead of showing "1024 KB".
        assert_eq!(fmt_bytes(MB - 1), "1 MB");
        assert_eq!(fmt_bytes(MB), "1 MB");
        assert_eq!(fmt_bytes(1023 * MB), "1023 MB");
        assert_eq!(fmt_bytes(GB - 1), "1.0 GB");
        assert_eq!(fmt_bytes(GB), "1.0 GB");
        assert_eq!(fmt_bytes(36 * GB), "36.0 GB");
        assert_eq!(fmt_bytes(97 * MB + 700 * KB), "98 MB");
        assert!(fmt_bytes(u64::MAX).ends_with(" GB"));
    }

    #[test]
    fn mem_approx_marker_only_on_a_number() {
        assert_eq!(fmt_mem(None, true, Lang::En), "—");
        assert_eq!(fmt_mem(Some(97 * MB), false, Lang::ZhTw), "97 MB");
        assert_eq!(fmt_mem(Some(97 * MB), true, Lang::ZhTw), "約 97 MB");
        assert_eq!(fmt_mem(Some(97 * MB), false, Lang::En), "97 MB");
    }

    #[test]
    fn cpu_rounding_and_odd_values() {
        assert_eq!(fmt_cpu(Some(0.0)), "0.0%");
        assert_eq!(fmt_cpu(Some(9.94)), "9.9%");
        assert_eq!(fmt_cpu(Some(9.96)), "10%"); // never "10.0%"
        assert_eq!(fmt_cpu(Some(10.0)), "10%");
        assert_eq!(fmt_cpu(Some(250.0)), "250%");
        assert_eq!(fmt_cpu(Some(-1.0)), "0.0%");
        assert_eq!(fmt_cpu(Some(f32::NAN)), "—");
        assert_eq!(fmt_cpu(Some(f32::INFINITY)), "—");
    }

    #[test]
    fn home_needs_a_path_boundary() {
        assert_eq!(shorten_home("/Users/u", Some("/Users/u")), "~");
        assert_eq!(shorten_home("/Users/u/", Some("/Users/u")), "~/");
        assert_eq!(shorten_home("/Users/u2/x", Some("/Users/u")), "/Users/u2/x");
        assert_eq!(shorten_home("/Users/u/x", Some("/Users/u/")), "~/x");
        assert_eq!(shorten_home("/Users/u/x", Some("/Users/u//")), "~/x");
        assert_eq!(shorten_home("/tmp/x", Some("/Users/u")), "/tmp/x");
        assert_eq!(shorten_home("/Users/u/x", None), "/Users/u/x");
        assert_eq!(shorten_home("/Users/u/x", Some("")), "/Users/u/x");
        // A root "home" must not turn every path into "~/...".
        assert_eq!(shorten_home("/x", Some("/")), "/x");
        // Unix paths are case-sensitive.
        assert_eq!(shorten_home("/users/u/x", Some("/Users/u")), "/users/u/x");
        // Already shortened or relative input is left alone.
        assert_eq!(shorten_home("~/x", Some("/Users/u")), "~/x");
    }

    #[test]
    fn windows_home_is_case_insensitive_and_bounded() {
        let home = Some("C:\\Users\\u");
        assert_eq!(shorten_home("c:\\users\\U\\prj", home), "~\\prj");
        assert_eq!(shorten_home("C:\\Users\\u", home), "~");
        assert_eq!(
            shorten_home("C:\\Users\\u2\\prj", home),
            "C:\\Users\\u2\\prj"
        );
        assert_eq!(shorten_home("D:\\Users\\u\\prj", home), "D:\\Users\\u\\prj");
        assert_eq!(shorten_home("C:/Users/u/prj", home), "~/prj");
        assert_eq!(
            shorten_home("C:\\Users\\u\\prj", Some("C:\\Users\\u\\")),
            "~\\prj"
        );
        assert_eq!(
            shorten_home("C:\\Users\\用戶\\prj", Some("c:\\users\\用戶")),
            "~\\prj"
        );
        assert_eq!(
            shorten_home("\\\\Srv\\home\\u\\x", Some("\\\\srv\\HOME\\u")),
            "~\\x"
        );
        // A backslash in a Unix name does not turn the home into a Windows one.
        assert_eq!(
            shorten_home("/home/A\\b/x", Some("/home/a\\b")),
            "/home/A\\b/x"
        );
        assert_eq!(shorten_home("/home/a\\b/x", Some("/home/a\\b")), "~/x");
    }

    #[test]
    fn age_units_and_edges() {
        let zh = |s, n| fmt_age(s, n, Lang::ZhTw);
        let en = |s, n| fmt_age(s, n, Lang::En);
        assert_eq!(zh(1000, 1000), "已執行不到 1 分鐘");
        assert_eq!(en(1000, 1059), "running <1 min");
        assert_eq!(zh(1000, 1060), "已執行 1 分鐘");
        assert_eq!(zh(1000, 1000 + 59 * 60 + 59), "已執行 59 分鐘");
        assert_eq!(zh(1000, 1000 + 3600), "已執行 1 小時");
        assert_eq!(en(1000, 1000 + 23 * 3600 + 3599), "running 23 h");
        assert_eq!(zh(1000, 1000 + 86_400), "已執行 1 天");
        assert_eq!(en(1000, 1000 + 3 * 86_400), "running 3 d");
        // A clock that went backwards, and an unknown start time, must not
        // produce nonsense such as "20000 days".
        assert_eq!(zh(2000, 1000), "已執行不到 1 分鐘");
        assert_eq!(zh(0, 1_700_000_000), "—");
        assert_eq!(en(0, 5), "—");
    }

    #[test]
    fn fit_width_edges() {
        assert_eq!(fit_width("anything", 0), "");
        assert_eq!(fit_width("", 3), "   ");
        assert_eq!(fit_width("", 0), "");
        assert_eq!(fit_width("abc", 3), "abc"); // exact fit: no ellipsis
        assert_eq!(fit_width("abcd", 3), "ab…");
        assert_eq!(fit_width("abcd", 1), "…");
        assert_eq!(fit_width("中文", 4), "中文");
        assert_eq!(fit_width("中文", 5), "中文 ");
        assert_eq!(fit_width("中文路", 4), "中… "); // 文 would cross the edge
        assert_eq!(fit_width("中中", 2), "… ");
        assert_eq!(fit_width("中中", 1), "…");
    }

    #[test]
    fn fit_width_keeps_combining_marks_with_their_base() {
        let s = "e\u{301}e\u{301}e\u{301}"; // 3 columns, 6 chars
        assert_eq!(fit_width(s, 3), s);
        assert_eq!(fit_width(s, 2), "e\u{301}…");
        assert_eq!(fit_width("e\u{301}", 3), "e\u{301}  ");
    }

    #[test]
    fn fit_width_counts_emoji_presentation_like_the_terminal() {
        // A heart plus U+FE0F is drawn 2 columns wide; the cell must not
        // spill over because each character was counted on its own.
        let hearts = "❤\u{fe0f}❤\u{fe0f}";
        assert_eq!(UnicodeWidthStr::width(hearts), 4);
        assert_eq!(fit_width(hearts, 4), hearts);
        assert_eq!(fit_width(hearts, 3), "❤\u{fe0f}…");
        assert_eq!(fit_width(hearts, 6), format!("{hearts}  "));
    }

    #[test]
    fn fit_width_flattens_control_characters() {
        assert_eq!(fit_width("a\tb\n", 6), "a b   ");
        assert_eq!(fit_width("x\u{1b}[31my", 8), "x [31my ");
    }

    #[test]
    fn fit_width_is_always_exactly_the_width() {
        let samples = [
            "",
            "a",
            "ab",
            "中文路徑很長的專案名稱",
            "~/prj/非常非常長的中文專案目錄名稱/子資料夾",
            "e\u{301}e\u{301}e\u{301}",
            "\u{301}\u{301}",
            "❤\u{fe0f}❤\u{fe0f}❤\u{fe0f}",
            "👨\u{200d}👩\u{200d}👧 family",
            "日本語テキスト",
            "한국어 텍스트",
            "tab\tnew\nline",
            "a\u{200b}b\u{200b}c",
            "🇹🇼🇯🇵",
        ];
        for s in samples {
            for w in 0..=14 {
                let out = fit_width(s, w);
                assert_eq!(UnicodeWidthStr::width(out.as_str()), w, "{s:?} at {w}");
                assert!(!out.chars().any(char::is_control), "{s:?} at {w}");
            }
        }
    }
}
