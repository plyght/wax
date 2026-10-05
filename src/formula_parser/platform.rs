use regex::Regex;
use std::sync::OnceLock;

#[derive(Clone, Copy)]
pub(crate) struct Target {
    pub mac: bool,
    pub arm: bool,
}

impl Target {
    pub(crate) fn host() -> Self {
        Self {
            mac: std::env::consts::OS == "macos",
            arm: std::env::consts::ARCH == "aarch64",
        }
    }

    fn predicate(self, name: &str) -> Option<bool> {
        Some(match name {
            "OS.mac?" => self.mac,
            "OS.linux?" => !self.mac,
            "Hardware::CPU.arm?" => self.arm,
            "Hardware::CPU.intel?" => !self.arm,
            "Hardware::CPU.is_64_bit?" => true,
            "Hardware::CPU.is_32_bit?" | "Hardware::CPU.in_rosetta2?" => false,
            "Hardware::CPU.avx2?" => !self.arm,
            _ => return None,
        })
    }

    fn block(self, name: &str) -> Option<bool> {
        Some(match name {
            "on_macos" => self.mac,
            "on_linux" => !self.mac,
            "on_arm" => self.arm,
            "on_intel" => !self.arm,
            "stable" => true,
            _ => return None,
        })
    }
}

struct Cond<'a> {
    tokens: Vec<&'a str>,
    pos: usize,
    target: Target,
}

impl<'a> Cond<'a> {
    fn eval(expr: &'a str, target: Target) -> Option<bool> {
        let re = TOKEN_RE
            .get_or_init(|| Regex::new(r"&&|\|\||!|\(|\)|[A-Za-z_][A-Za-z0-9_:.]*\??").unwrap());
        let tokens: Vec<&str> = re.find_iter(expr).map(|m| m.as_str()).collect();
        let mut cond = Cond {
            tokens,
            pos: 0,
            target,
        };
        let value = cond.or()?;
        (cond.pos == cond.tokens.len()).then_some(value)
    }

    fn peek(&self) -> Option<&'a str> {
        self.tokens.get(self.pos).copied()
    }

    fn or(&mut self) -> Option<bool> {
        let mut value = self.and()?;
        while matches!(self.peek(), Some("||" | "or")) {
            self.pos += 1;
            value |= self.and()?;
        }
        Some(value)
    }

    fn and(&mut self) -> Option<bool> {
        let mut value = self.unary()?;
        while matches!(self.peek(), Some("&&" | "and")) {
            self.pos += 1;
            value &= self.unary()?;
        }
        Some(value)
    }

    fn unary(&mut self) -> Option<bool> {
        match self.peek()? {
            "!" | "not" => {
                self.pos += 1;
                Some(!self.unary()?)
            }
            "(" => {
                self.pos += 1;
                let value = self.or()?;
                (self.peek()? == ")").then(|| self.pos += 1)?;
                Some(value)
            }
            name => {
                self.pos += 1;
                self.target.predicate(name)
            }
        }
    }
}

static TOKEN_RE: OnceLock<Regex> = OnceLock::new();
static URL_RE: OnceLock<Regex> = OnceLock::new();
static SHA_RE: OnceLock<Regex> = OnceLock::new();
static VERSION_RE: OnceLock<Regex> = OnceLock::new();
static DO_RE: OnceLock<Regex> = OnceLock::new();
static HEREDOC_RE: OnceLock<Regex> = OnceLock::new();

enum Frame {
    Block {
        active: bool,
    },
    If {
        parent: bool,
        taken: bool,
        active: bool,
    },
}

impl Frame {
    fn active(&self) -> bool {
        match self {
            Frame::Block { active } | Frame::If { active, .. } => *active,
        }
    }
}

fn split_modifier(line: &str) -> (&str, Option<(&str, bool)>) {
    for (keyword, negate) in [(" if ", false), (" unless ", true)] {
        if let Some(idx) = line.rfind(keyword) {
            if line[..idx].matches('"').count().is_multiple_of(2) {
                return (&line[..idx], Some((&line[idx + keyword.len()..], negate)));
            }
        }
    }
    (line, None)
}

pub(crate) fn select_url_sha(content: &str, target: Target) -> Option<(String, String)> {
    let url_re = URL_RE.get_or_init(|| Regex::new(r#"^url\s+"([^"]+)""#).unwrap());
    let sha_re = SHA_RE.get_or_init(|| Regex::new(r#"^sha256\s+"([0-9a-fA-F]{64})""#).unwrap());
    let version_re = VERSION_RE.get_or_init(|| Regex::new(r#"^version\s+"([^"]+)""#).unwrap());
    let do_re = DO_RE.get_or_init(|| Regex::new(r"^([a-z_]*).*\bdo(\s*\|[^|]*\|)?$").unwrap());
    let heredoc_re = HEREDOC_RE.get_or_init(|| Regex::new(r"<<[~-]?([A-Z_]+)").unwrap());

    let mut stack: Vec<Frame> = Vec::new();
    let mut heredoc: Option<String> = None;
    let mut version: Option<String> = None;
    let mut url: Option<String> = None;
    let mut sha: Option<String> = None;
    let mut conditional = false;
    let mut entered_class = false;

    for raw in content.lines() {
        let line = raw.trim();
        if let Some(term) = &heredoc {
            if line == term {
                heredoc = None;
            }
            continue;
        }
        if let Some(cap) = heredoc_re.captures(line) {
            heredoc = Some(cap[1].to_string());
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !entered_class {
            entered_class = line.starts_with("class ");
            continue;
        }
        let active = stack.iter().all(Frame::active);

        if line == "end" || line.starts_with("end ") || line.starts_with("end#") {
            if stack.pop().is_none() {
                break;
            }
            continue;
        }
        if let Some(cond) = line
            .strip_prefix("if ")
            .or_else(|| line.strip_prefix("unless "))
        {
            let value = Cond::eval(cond, target).map(|v| v ^ line.starts_with("unless "));
            let taken = value.unwrap_or(true);
            stack.push(Frame::If {
                parent: active,
                taken,
                active: active && value == Some(true),
            });
            continue;
        }
        if let Some(cond) = line.strip_prefix("elsif ") {
            if let Some(Frame::If {
                parent,
                taken,
                active: branch,
            }) = stack.last_mut()
            {
                let value = Cond::eval(cond, target);
                *branch = *parent && !*taken && value == Some(true);
                *taken |= value.unwrap_or(true);
            }
            continue;
        }
        if line == "else" {
            if let Some(Frame::If {
                parent,
                taken,
                active: branch,
            }) = stack.last_mut()
            {
                *branch = *parent && !*taken;
                *taken = true;
            }
            continue;
        }
        if line.starts_with("def ") {
            if !line.contains(" = ") {
                stack.push(Frame::Block { active: false });
            }
            continue;
        }
        if let Some(cap) = do_re.captures(line) {
            let known = target.block(&cap[1]);
            stack.push(Frame::Block {
                active: active && known == Some(true),
            });
            continue;
        }
        if line.starts_with("case ") || line.starts_with("begin") || line.starts_with("while ") {
            stack.push(Frame::Block { active: false });
            continue;
        }
        if !active {
            continue;
        }

        let (stmt, modifier) = split_modifier(line);
        if let Some((cond, negate)) = modifier {
            if Cond::eval(cond, target).map(|v| v ^ negate) != Some(true) {
                continue;
            }
        }
        if stack.is_empty() {
            if let Some(cap) = version_re.captures(stmt) {
                version = Some(cap[1].to_string());
            }
        }
        if let Some(cap) = url_re.captures(stmt) {
            url = Some(cap[1].to_string());
            sha = None;
            conditional = !stack.is_empty() || modifier.is_some();
        } else if let Some(cap) = sha_re.captures(stmt) {
            sha = Some(cap[1].to_lowercase());
        }
    }

    if !conditional {
        return None;
    }
    let mut url = url?;
    if let Some(version) = &version {
        url = url.replace("#{version}", version);
    }
    if url.contains("#{") {
        return None;
    }
    Some((url, sha?))
}

#[cfg(test)]
mod tests;
