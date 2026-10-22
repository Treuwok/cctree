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
    let s = mask_embedded_assignments(&s);
    let s = mask_embedded_flag_values(&s);
    let s = mask_header_values(&s);
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

/// A one-word flag with no `=` (`--access-token`) whose name says it carries a
/// secret: the next argument is the value.
fn is_secret_flag(arg: &str) -> bool {
    arg.starts_with('-')
        && !arg.contains(|c: char| c == '=' || c.is_whitespace())
        && has_secret_name(arg)
}

fn starts_with_secret_prefix(s: &str) -> bool {
    SECRET_PREFIXES.iter().any(|p| s.starts_with(p))
}

/// Shortest user part (without a password) that is treated as a token, e.g. the
/// 32-hex public key in a Sentry DSN. `git` or `user` stay readable.
const TOKEN_LIKE_USER_LEN: usize = 16;

fn is_token_like_user(userinfo: &str) -> bool {
    userinfo.len() >= TOKEN_LIKE_USER_LEN
        && userinfo
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `https://user:pass@host` becomes `https://***@host`. A user name alone
/// (`ssh://git@host`) is left alone unless it looks like a token (known prefix,
/// or a long run of `[A-Za-z0-9_-]`).
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
            if userinfo.contains(':')
                || starts_with_secret_prefix(userinfo)
                || is_token_like_user(userinfo)
            {
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

/// A `NAME=value` may start at the beginning of the text or right after
/// whitespace, a quote (`'` `"` `` ` ``), `;`, `&`, `|`, `(` or `?` (URL query).
fn is_assignment_boundary(before: Option<char>) -> bool {
    before.is_none_or(|c| {
        c.is_whitespace() || matches!(c, '\'' | '"' | '`' | ';' | '&' | '|' | '(' | '?')
    })
}

/// If `s` starts with `[-]{0,2}NAME=` where `NAME` is `[A-Za-z_][A-Za-z0-9_-]*`
/// and says it carries a secret, returns the byte length of that head (through `=`).
fn secret_assignment_head(s: &str) -> Option<usize> {
    let dashes = s.bytes().take(2).take_while(|&b| b == b'-').count();
    let name_and_rest = &s[dashes..];
    let first = name_and_rest.chars().next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let name_len = name_and_rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(name_and_rest.len());
    if !name_and_rest[name_len..].starts_with('=') || !has_secret_name(&name_and_rest[..name_len]) {
        return None;
    }
    Some(dashes + name_len + 1)
}

/// Returns `(opening quote length, value length)` for the value at the start of `s`.
/// A quoted value is the quoted content (an unterminated quote runs to the end);
/// anything else ends at whitespace, a quote, `;`, `&`, `|` or `)`.
fn assignment_value(s: &str) -> (usize, usize) {
    match s.chars().next() {
        Some(quote @ ('\'' | '"')) => (1, s[1..].find(quote).unwrap_or(s.len() - 1)),
        _ => {
            let end = s.find(|c: char| {
                c.is_whitespace() || matches!(c, '\'' | '"' | '`' | ';' | '&' | '|' | ')')
            });
            (0, end.unwrap_or(s.len()))
        }
    }
}

/// Masks `NAME=value` anywhere in the text when `NAME` contains key/token/secret/
/// password/auth, e.g. `export CLAUDE_CODE_MESSAGING_TOKEN=abc && ...` inside a
/// `sh -c` string, `env GITHUB_TOKEN=abc cmd`, or `?token=abc` in a URL. Quotes
/// around the value are kept; empty values are left alone.
fn mask_embedded_assignments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut copied = 0;
    let mut resume = 0;
    for (i, _) in s.char_indices() {
        if i < resume || !is_assignment_boundary(s[..i].chars().next_back()) {
            continue;
        }
        let Some(head) = secret_assignment_head(&s[i..]) else {
            continue;
        };
        let value_start = i + head;
        let (quote_len, value_len) = assignment_value(&s[value_start..]);
        let content_start = value_start + quote_len;
        resume = content_start + value_len;
        if value_len > 0 {
            out.push_str(&s[copied..content_start]);
            out.push_str(MASK);
            copied = resume;
        }
    }
    out.push_str(&s[copied..]);
    out
}

/// The rule [`mask_argv`] applies to separate arguments, for a command kept as
/// one argument (`sh -c "tool --api-key abc"`): the word after a secret-named
/// flag is masked, quotes kept. Decided from the original text, so
/// `--token --password x` masks both values.
fn mask_embedded_flag_values(s: &str) -> String {
    let mut values: Vec<(usize, usize)> = Vec::new();
    for (i, _) in s.char_indices().filter(|&(_, c)| c == '-') {
        if !is_assignment_boundary(s[..i].chars().next_back()) {
            continue;
        }
        let flag_end = i + secret_len(&s[i..]);
        if !is_secret_flag(&s[i..flag_end]) {
            continue;
        }
        let after = &s[flag_end..];
        let value_start = flag_end + (after.len() - after.trim_start().len());
        if value_start == flag_end {
            continue;
        }
        let (quote_len, value_len) = assignment_value(&s[value_start..]);
        if value_len > 0 {
            let start = value_start + quote_len;
            values.push((start, start + value_len));
        }
    }
    let mut out = String::with_capacity(s.len());
    let mut copied = 0;
    for (start, end) in values {
        if start < copied {
            continue;
        }
        out.push_str(&s[copied..start]);
        out.push_str(MASK);
        copied = end;
    }
    out.push_str(&s[copied..]);
    out
}

/// Authorization schemes kept in front of a masked header value.
const AUTH_SCHEMES: &[&str] = &["basic", "bearer", "digest", "token"];

/// If `s` starts with `NAME:` where `NAME` is `[A-Za-z][A-Za-z0-9_-]*` and says
/// it carries a secret, returns the byte length of that head (through `:`).
/// `NAME://` is a URL, not a header.
fn secret_header_head(s: &str) -> Option<usize> {
    if !s.chars().next()?.is_ascii_alphabetic() {
        return None;
    }
    let name_len = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(s.len());
    let rest = &s[name_len..];
    if !rest.starts_with(':') || rest.starts_with("://") || !has_secret_name(&s[..name_len]) {
        return None;
    }
    Some(name_len + 1)
}

/// Masks the value of an HTTP header whose name contains key/token/secret/
/// password/auth: `X-Api-Key: abc` becomes `X-Api-Key: ***`, also without the
/// space (`mcp-remote --header X-Api-Key:abc`), after `--header=`, or inside a
/// `sh -c` string. An authorization scheme is kept, so `Authorization: Basic
/// abc` becomes `Authorization: Basic ***`.
fn mask_header_values(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut copied = 0;
    let mut resume = 0;
    for (i, _) in s.char_indices() {
        let before = s[..i].chars().next_back();
        if i < resume || !(is_assignment_boundary(before) || before == Some('=')) {
            continue;
        }
        let Some(head) = secret_header_head(&s[i..]) else {
            continue;
        };
        let after_colon = i + head;
        let mut value_start =
            after_colon + (s[after_colon..].len() - s[after_colon..].trim_start().len());
        let word_len = s[value_start..]
            .find(char::is_whitespace)
            .unwrap_or(s.len() - value_start);
        let word = &s[value_start..value_start + word_len];
        if AUTH_SCHEMES.iter().any(|a| word.eq_ignore_ascii_case(a)) {
            let rest = &s[value_start + word_len..];
            value_start += word_len + (rest.len() - rest.trim_start().len());
        }
        let value_len = secret_len(&s[value_start..]);
        resume = value_start + value_len;
        if value_len > 0 {
            out.push_str(&s[copied..value_start]);
            out.push_str(MASK);
            copied = resume;
        }
    }
    out.push_str(&s[copied..]);
    out
}

/// `--github-token=abc` becomes `--github-token=***`. Covers any flag name
/// (including characters `NAME` does not allow, like `-Dspring.db.password=x`)
/// and masks the whole rest of the argument.
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
            "X-Api-Key: k1",
            "Authorization: Basic b64",
            "tool --api-key k2 --token --password 'p q'",
        ];
        let once = m(&all);
        assert_eq!(mask_argv(&once), once);
    }

    // PR review: a secret flag and its value inside one argument (`sh -c "..."`).
    #[test]
    fn masks_secret_flag_values_inside_a_shell_command() {
        assert_eq!(
            m(&["sh", "-c", "tool --api-key customer-secret --verbose"]),
            ["sh", "-c", "tool --api-key *** --verbose"]
        );
        assert_eq!(
            m(&["zsh", "-c", "x --token --password 'p q' && y"]),
            ["zsh", "-c", "x --token *** '***' && y"]
        );
        for s in [
            "git log --oneline -5",
            "npm run build --key",
            "foo-token bar",
        ] {
            assert_eq!(m(&[s]), [s]);
        }
    }

    // PR review: credentials passed as HTTP headers (`--header`, `-H`, `sh -c "curl -H ..."`).
    #[test]
    fn masks_secret_header_values() {
        assert_eq!(
            m(&[
                "mcp-remote",
                "https://x/mcp",
                "--header",
                "X-Api-Key: customer-secret"
            ]),
            ["mcp-remote", "https://x/mcp", "--header", "X-Api-Key: ***"]
        );
        assert_eq!(m(&["X-Auth-Token:abc123"]), ["X-Auth-Token:***"]);
        assert_eq!(m(&["--header=X-API-KEY: abc"]), ["--header=X-API-KEY: ***"]);
        assert_eq!(
            m(&["-H", "Authorization: Basic dXNlcjpwYXNz"]),
            ["-H", "Authorization: Basic ***"]
        );
        assert_eq!(
            m(&["sh", "-c", "curl -H 'X-Api-Key: abc' https://x"]),
            ["sh", "-c", "curl -H 'X-Api-Key: ***' https://x"]
        );
        for s in [
            "Content-Type: application/json",
            "https://host:8080/x",
            "key://x",
        ] {
            assert_eq!(m(&[s]), [s]);
        }
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

    // Recon leak 1 (R-c): `NAME=value` anywhere inside an argument, not only in `--flag=value`.
    #[test]
    fn masks_secret_assignments_inside_a_shell_command() {
        let once = m(&[
            "/bin/zsh",
            "-c",
            "source /x.sh || true && export CLAUDE_CODE_MESSAGING_TOKEN=abc123def \
             && export CODEX_COMPANION_SESSION_ID='u-1' && eval 'ls'",
        ]);
        assert_eq!(
            once[2],
            "source /x.sh || true && export CLAUDE_CODE_MESSAGING_TOKEN=*** \
             && export CODEX_COMPANION_SESSION_ID='u-1' && eval 'ls'"
        );
        assert_eq!(mask_argv(&once), once);
    }

    #[test]
    fn masks_bare_and_embedded_secret_assignments() {
        let cases: [(&[&str], &[&str]); 4] = [
            (
                &["env", "GITHUB_TOKEN=abc", "node", "x.js"],
                &["env", "GITHUB_TOKEN=***", "node", "x.js"],
            ),
            (
                &["sh", "-c", "API_KEY=\"abc def\" run"],
                &["sh", "-c", "API_KEY=\"***\" run"],
            ),
            (
                &["sh", "-c", "X_TOKEN='abc def' run"],
                &["sh", "-c", "X_TOKEN='***' run"],
            ),
            (
                &["a;X_SECRET=abc;b", "a&&Db-Password=abc&&b", "x|AUTH=abc|y"],
                &["a;X_SECRET=***;b", "a&&Db-Password=***&&b", "x|AUTH=***|y"],
            ),
        ];
        for (input, expected) in cases {
            let once = m(input);
            assert_eq!(once, expected, "input {input:?}");
            assert_eq!(mask_argv(&once), once, "not idempotent for {input:?}");
        }
    }

    #[test]
    fn masks_secret_assignments_after_each_boundary() {
        let cases = [
            ("(X_PASSWORD=abc)", "(X_PASSWORD=***)"),
            ("'X_KEY=abc'", "'X_KEY=***'"),
            ("`X_KEY=abc`", "`X_KEY=***`"),
            ("curl --client-secret=abc x", "curl --client-secret=*** x"),
            ("curl -token=abc x", "curl -token=*** x"),
            ("https://h/p?token=abc&y=1", "https://h/p?token=***&y=1"),
            ("https://h/p?y=1&auth=abc", "https://h/p?y=1&auth=***"),
            ("X_TOKEN=\"abc def", "X_TOKEN=\"***"),
            ("X_TOKEN=abc;Y_TOKEN=def", "X_TOKEN=***;Y_TOKEN=***"),
        ];
        for (input, expected) in cases {
            let once = m(&[input]);
            assert_eq!(once[0], expected, "input {input:?}");
            assert_eq!(mask_argv(&once), once, "not idempotent for {input:?}");
        }
    }

    #[test]
    fn plain_assignments_and_empty_values_are_kept() {
        for s in [
            "PATH=/usr/bin",
            "--lang=en",
            "LANG=en_US.UTF-8",
            "CODEX_COMPANION_SESSION_ID='u-1'",
            "X_TOKEN=",
            "X_TOKEN=''",
            "/x/TOKEN=1",
            "a.TOKEN=1",
        ] {
            assert_eq!(m(&[s]), [s], "input {s:?}");
        }
    }

    // Recon leak 2 (R-d): URL user part without `:` that looks like a token (Sentry DSN key).
    #[test]
    fn masks_token_like_url_user() {
        let once = m(&["--url=https://0123456789abcdef0123456789abcdef@o1.ingest.sentry.io/2"]);
        assert_eq!(once, ["--url=https://***@o1.ingest.sentry.io/2"]);
        assert_eq!(mask_argv(&once), once);
        assert_eq!(
            m(&[
                "https://abcdefghijklmnop@h/x",
                "https://A_b-C_d-E_f-G_h-@h/x"
            ]),
            ["https://***@h/x", "https://***@h/x"]
        );
    }

    // Every short combination of the rules' building blocks must neither panic
    // (byte slicing on non-ASCII text) nor change when masked a second time.
    #[test]
    fn small_inputs_never_panic_and_are_idempotent() {
        let atoms = [
            "--", "=", "'", "\"", " ", ": ", "@", "://", "&", "?", ";", "(", "TOKEN", "sk-",
            "ghp_", "Bearer", "日",
        ];
        let mut inputs = vec![String::new()];
        let mut frontier = vec![String::new()];
        for _ in 0..4 {
            let mut next = Vec::new();
            for prefix in &frontier {
                for atom in atoms {
                    next.push(format!("{prefix}{atom}"));
                }
            }
            inputs.extend(next.iter().cloned());
            frontier = next;
        }
        for input in inputs {
            let once = mask_argv(std::slice::from_ref(&input));
            assert_eq!(mask_argv(&once), once, "not idempotent for {input:?}");
            let pair = mask_argv(&[input.clone(), input.clone()]);
            assert_eq!(mask_argv(&pair), pair, "not idempotent for pair {input:?}");
        }
    }

    #[test]
    fn short_or_plain_url_users_are_kept() {
        for s in [
            "git@github.com:o/r.git",
            "ssh://git@host/x",
            "https://user@host/x",
            "https://abcdefghijklmno@h/x",
            "https://first.last.name.long@h/x",
        ] {
            assert_eq!(m(&[s]), [s], "input {s:?}");
        }
    }
}
