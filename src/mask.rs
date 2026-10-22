//! Masks secrets in command lines before they are shown.
//!
//! Every command line the UI displays goes through [`mask_argv`]; there is no
//! way to show the original. The rules use plain string handling (no regex)
//! and are idempotent: masking an already masked command line changes nothing.

/// What a masked value is replaced with.
const MASK: &str = "***";

/// A flag is secret-bearing when its lowercased name contains one of these. For
/// `--name=value` the value is masked; for `--name value` the next argument is
/// (so `--access-token abc` and `--TOKEN abc` are covered, not just five flags).
/// This also catches display-only false positives such as `--author x`.
const SECRET_NAME_PARTS: &[&str] = &["key", "token", "secret", "password", "auth"];

/// Well-known secret prefixes. They only count at a word start (see
/// [`is_prefix_boundary`]), never as an arbitrary substring: otherwise
/// `--disk-cache-size` (which contains `sk-`) would be masked too.
const SECRET_PREFIXES: &[&str] = &[
    "sk-",
    "ctx7sk-",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "xox",
    "AKIA",
];

/// Returns `argv` with every secret replaced by `***`.
pub fn mask_argv(argv: &[String]) -> Vec<String> {
    let mut masked = Vec::with_capacity(argv.len());
    let mut mask_next = false;
    for arg in argv {
        masked.push(if mask_next {
            MASK.to_string()
        } else {
            mask_arg(arg)
        });
        // Decided from the original argument, so `--token --password x` masks both values.
        mask_next = is_secret_flag(arg) || ends_with_bearer(arg);
    }
    masked
}

fn mask_arg(arg: &str) -> String {
    let s = mask_url_userinfo(arg);
    let s = mask_bearer(&s);
    let s = mask_named_assignment(&s);
    mask_known_prefixes(&s)
}

/// Where a secret value stops: whitespace, `&`, `;`, a quote, or a closing bracket.
fn secret_len(s: &str) -> usize {
    s.find(|c: char| {
        c.is_whitespace() || matches!(c, '&' | ';' | '"' | '\'' | '`' | ')' | ']' | '}')
    })
    .unwrap_or(s.len())
}

fn has_secret_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SECRET_NAME_PARTS.iter().any(|part| lower.contains(part))
}

/// A flag with no `=` (`--access-token`) whose name says it carries a secret:
/// the next argument is the value.
fn is_secret_flag(arg: &str) -> bool {
    arg.starts_with('-') && !arg.contains('=') && has_secret_name(arg)
}

fn starts_with_secret_prefix(s: &str) -> bool {
    SECRET_PREFIXES.iter().any(|p| s.starts_with(p))
}

/// `https://user:pass@host` becomes `https://***@host`. A user name alone
/// (`ssh://git@host`) is left alone unless it looks like a token.
fn mask_url_userinfo(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find("://") {
        let authority_start = pos + 3;
        out.push_str(&rest[..authority_start]);
        rest = &rest[authority_start..];
        let authority_len = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '?' | '#' | '"' | '\''))
            .unwrap_or(rest.len());
        let authority = &rest[..authority_len];
        if let Some(at) = authority.rfind('@') {
            let userinfo = &authority[..at];
            if userinfo.contains(':') || starts_with_secret_prefix(userinfo) {
                out.push_str(MASK);
                out.push_str(&authority[at..]);
                rest = &rest[authority_len..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `Authorization: Bearer abc` becomes `Authorization: Bearer ***`.
fn mask_bearer(s: &str) -> String {
    const WORD: &str = "bearer";
    // ASCII lowercasing keeps byte offsets identical to `s`.
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut copied = 0;
    let mut search = 0;
    while let Some(found) = lower[search..].find(WORD) {
        let start = search + found;
        let end = start + WORD.len();
        search = end;
        let word_start = !s[..start]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric);
        let spaces = s[end..].len() - s[end..].trim_start().len();
        if !word_start || spaces == 0 {
            continue;
        }
        let value_start = end + spaces;
        let value_len = secret_len(&s[value_start..]);
        if value_len == 0 {
            continue;
        }
        out.push_str(&s[copied..value_start]);
        out.push_str(MASK);
        copied = value_start + value_len;
        search = copied;
    }
    out.push_str(&s[copied..]);
    out
}

/// True when the argument ends with the word `Bearer`, so the next argument is the credential.
fn ends_with_bearer(arg: &str) -> bool {
    arg.split_whitespace()
        .next_back()
        .is_some_and(|w| w.eq_ignore_ascii_case("bearer"))
}

/// `--github-token=abc` becomes `--github-token=***`.
fn mask_named_assignment(arg: &str) -> String {
    if arg.starts_with('-')
        && let Some((name, _)) = arg.split_once('=')
        && has_secret_name(name)
    {
        return format!("{name}={MASK}");
    }
    arg.to_string()
}

/// A known prefix counts at the start of the argument or right after whitespace,
/// a quote (`'` `"` `` ` ``), `=`, `:` or an opening/separating bracket (`,` `(` `[` `{`).
/// It does not count after a letter, digit, `-`, `_`, `/` or `.`, so
/// `--disk-cache-size`, `task-runner` and `/x/risk-report.js` stay readable.
fn is_prefix_boundary(before: Option<char>) -> bool {
    before.is_none_or(|c| {
        c.is_whitespace() || matches!(c, '\'' | '"' | '`' | '=' | ':' | ',' | '(' | '[' | '{')
    })
}

/// Masks well-known secret prefixes wherever they start a word (see [`is_prefix_boundary`]).
fn mask_known_prefixes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut skip_until = 0;
    for (i, c) in s.char_indices() {
        if i < skip_until {
            continue;
        }
        if is_prefix_boundary(s[..i].chars().next_back()) && starts_with_secret_prefix(&s[i..]) {
            out.push_str(MASK);
            skip_until = i + secret_len(&s[i..]);
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(v: &[&str]) -> Vec<String> {
        mask_argv(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }
    #[test]
    fn masks_flag_values() {
        assert_eq!(
            m(&["ctx", "--api-key", "ctx7sk-abc"]),
            ["ctx", "--api-key", "***"]
        );
        for f in ["--token", "--password", "--secret", "--key"] {
            assert_eq!(m(&[f, "v"])[1], "***");
        }
    }
    #[test]
    fn masks_assignment_forms_by_name() {
        assert_eq!(m(&["--github-token=abc"]), ["--github-token=***"]);
        assert_eq!(m(&["--AUTH_HEADER=x"]), ["--AUTH_HEADER=***"]);
        assert_eq!(m(&["--db-password=p"]), ["--db-password=***"]);
    }
    #[test]
    fn masks_known_prefixes_at_token_start() {
        for s in [
            "sk-proj-123",
            "ctx7sk-1",
            "ghp_x",
            "gho_y",
            "xoxb-1",
            "AKIAABC",
        ] {
            assert_eq!(m(&[s]), ["***"]);
        }
        assert_eq!(m(&["KEY=sk-123"]), ["KEY=***"]);
        for s in ["--disk-cache-size=1", "task-runner", "/x/risk-report.js"] {
            assert_eq!(m(&[s]), [s]);
        }
    }
    #[test]
    fn masks_url_userinfo_and_bearer() {
        assert_eq!(m(&["https://me:pw@host/x"]), ["https://***@host/x"]);
        assert_eq!(
            m(&["-H", "Authorization: Bearer abc"]),
            ["-H", "Authorization: Bearer ***"]
        );
        assert_eq!(m(&["Bearer", "abc"]), ["Bearer", "***"]);
    }
    #[test]
    fn leaves_normal_args_alone() {
        let v = [
            "npm",
            "exec",
            "chrome-devtools-mcp@latest",
            "--browserUrl",
            "http://127.0.0.1:9222",
            "--autoConnect",
        ];
        assert_eq!(m(&v), v);
        assert_eq!(m(&[]), Vec::<String>::new());
    }
    #[test]
    fn masking_is_idempotent() {
        let once = m(&["--token", "x", "sk-1"]);
        assert_eq!(mask_argv(&once), once);
    }

    #[test]
    fn masks_secrets_inside_longer_strings() {
        assert_eq!(
            m(&["https://x/?a=sk-1&b=ghp_2&c=3"]),
            ["https://x/?a=***&b=***&c=3"]
        );
        assert_eq!(
            m(&["export T=sk-1; curl -H 'x-api-key: AKIAZZ' https://h"]),
            ["export T=***; curl -H 'x-api-key: ***' https://h"]
        );
        assert_eq!(
            m(&["curl", "-H", "authorization: bearer abc.def-1"]),
            ["curl", "-H", "authorization: bearer ***"]
        );
        assert_eq!(
            m(&["https://a:b@h1/x https://c:d@h2/y"]),
            ["https://***@h1/x https://***@h2/y"]
        );
    }

    #[test]
    fn masks_token_used_as_url_user_but_keeps_plain_user() {
        assert_eq!(
            m(&["https://ghp_abc@github.com/o/r"]),
            ["https://***@github.com/o/r"]
        );
        assert_eq!(
            m(&["ssh://git@github.com/o/r", "git@github.com:o/r.git"]),
            ["ssh://git@github.com/o/r", "git@github.com:o/r.git"]
        );
    }

    #[test]
    fn flag_chain_masks_every_value() {
        assert_eq!(
            m(&["--token", "--password", "x", "ok"]),
            ["--token", "***", "***", "ok"]
        );
        assert_eq!(m(&["--token"]), ["--token"]);
    }

    #[test]
    fn handles_non_ascii_and_odd_input() {
        assert_eq!(
            m(&["日本語", "--名前=値", "日本 Bearer 値", "--token=日本"]),
            ["日本語", "--名前=値", "日本 Bearer ***", "--token=***"]
        );
        for s in ["", "=", ": ", "Bearer ", "Bearer", "https://", "https://@"] {
            let once = m(&[s]);
            assert_eq!(mask_argv(&once), once, "input {s:?}");
        }
    }

    #[test]
    fn masking_is_idempotent_for_every_rule() {
        let all = [
            "--token",
            "t",
            "--api-key=k",
            "KEY=sk-1",
            "x: ghp_1",
            "https://u:p@h/x",
            "Bearer",
            "abc",
            "H: Bearer zzz",
            "--access-token",
            "a",
            "bash",
            "-c",
            "curl --token sk-abc",
            "{\"api_key\":\"sk-1\"}",
        ];
        let once = m(&all);
        assert_eq!(mask_argv(&once), once);
    }

    // Review finding 1: a known prefix inside a longer argument (sh -c, JSON, headers).
    #[test]
    fn masks_prefixes_inside_longer_arguments() {
        let cases = [
            ("curl --token sk-abc", "curl --token ***"),
            ("Authorization: token ghp_x", "Authorization: token ***"),
            ("{\"api_key\":\"sk-1\"}", "{\"api_key\":\"***\"}"),
            ("x-api-key:AKIAABC", "x-api-key:***"),
            ("'sk-1'", "'***'"),
            ("\"sk-1\"", "\"***\""),
            ("`sk-1`", "`***`"),
            ("a,sk-1", "a,***"),
            ("(sk-1)", "(***)"),
            ("[sk-1]", "[***]"),
            ("{sk-1}", "{***}"),
            ("key:  ghp_x", "key:  ***"),
            ("echo xoxb-1 ctx7sk-2", "echo *** ***"),
        ];
        for (input, expected) in cases {
            assert_eq!(m(&["bash", "-c", input])[2], expected, "input {input:?}");
            let once = m(&[input]);
            assert_eq!(once[0], expected, "input {input:?}");
            assert_eq!(mask_argv(&once), once, "not idempotent for {input:?}");
        }
        assert_eq!(
            m(&["bash", "-c", "curl --token sk-abc"]),
            ["bash", "-c", "curl --token ***"]
        );
    }

    #[test]
    fn prefix_after_word_characters_is_not_a_secret() {
        for s in [
            "a-sk-1",
            "a_sk-1",
            "1sk-1",
            "/sk-1",
            ".sk-1",
            "x.ghp_1",
            "--disk-cache-size=1",
            "task-runner",
            "/x/risk-report.js",
            "--max-sk-1",
            "mask-xox",
            "taskAKIA",
        ] {
            assert_eq!(m(&[s]), [s], "input {s:?}");
        }
    }

    // Review finding 2: space-separated flags are matched by name, like the `=` form.
    #[test]
    fn masks_next_argument_of_any_secret_named_flag() {
        for f in [
            "--access-token",
            "--auth-token",
            "--client-secret",
            "--github-token",
            "--TOKEN",
            "--Api-Key",
            "--db-password",
            "-token",
        ] {
            assert_eq!(m(&[f, "abc"]), [f, "***"], "flag {f:?}");
            let once = m(&[f, "abc", "tail"]);
            assert_eq!(once, [f, "***", "tail"], "flag {f:?}");
            assert_eq!(mask_argv(&once), once, "not idempotent for {f:?}");
        }
    }

    #[test]
    fn non_secret_flags_keep_their_values() {
        let v = [
            "--port",
            "8080",
            "--disk-cache-size",
            "1",
            "--browserUrl",
            "http://127.0.0.1:9222",
            "--autoConnect",
            "--max-old-space-size",
            "4096",
            "-p",
            "x",
            "-",
        ];
        assert_eq!(m(&v), v);
    }
}
