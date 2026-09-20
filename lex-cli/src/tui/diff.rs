/// 基础行级 diff:公共前缀/后缀保持,中段按"删-增"成对输出。
/// 不做 LCS 与语法解析(Phase 6 任务书:先做最基础的着色即可)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    Add(String),
    Del(String),
    Ctx(String),
}

const MAX_DIFF_LINES: usize = 400;

pub fn line_diff(old: &str, new: &str) -> Vec<DiffLine> {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    // 公共前缀
    let mut prefix = 0usize;
    while prefix < old_lines.len() && prefix < new_lines.len() && old_lines[prefix] == new_lines[prefix] {
        prefix += 1;
    }
    // 公共后缀(不越过前缀)
    let mut suffix = 0usize;
    while suffix < old_lines.len() - prefix
        && suffix < new_lines.len() - prefix
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let old_mid = &old_lines[prefix..old_lines.len() - suffix];
    let new_mid = &new_lines[prefix..new_lines.len() - suffix];

    let mut out: Vec<DiffLine> = Vec::new();
    for l in &old_lines[..prefix] {
        out.push(DiffLine::Ctx((*l).to_string()));
    }
    for l in old_mid {
        out.push(DiffLine::Del((*l).to_string()));
    }
    for l in new_mid {
        out.push(DiffLine::Add((*l).to_string()));
    }
    for l in &old_lines[old_lines.len() - suffix..] {
        out.push(DiffLine::Ctx((*l).to_string()));
    }
    if out.len() > MAX_DIFF_LINES {
        out.truncate(MAX_DIFF_LINES);
        out.push(DiffLine::Del("…(diff 已截断)".into()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(v: &[DiffLine]) -> Vec<&str> {
        v.iter()
            .map(|l| match l {
                DiffLine::Add(_) => "+",
                DiffLine::Del(_) => "-",
                DiffLine::Ctx(_) => " ",
            })
            .collect()
    }

    #[test]
    fn identical_input_is_all_context() {
        let d = line_diff("a\nb\n", "a\nb\n");
        assert_eq!(kinds(&d), vec![" ", " "]);
    }

    #[test]
    fn middle_replacement_yields_del_then_add() {
        let d = line_diff("fn a() {}\nfn old() {}\nfn c() {}", "fn a() {}\nfn new() {}\nfn c() {}");
        assert_eq!(kinds(&d), vec![" ", "-", "+", " "]);
        assert_eq!(d[1], DiffLine::Del("fn old() {}".into()));
        assert_eq!(d[2], DiffLine::Add("fn new() {}".into()));
    }

    #[test]
    fn pure_append_keeps_prefix_context() {
        let d = line_diff("a\nb", "a\nb\nc");
        assert_eq!(kinds(&d), vec![" ", " ", "+"]);
        assert_eq!(d[2], DiffLine::Add("c".into()));
    }

    #[test]
    fn empty_to_content_is_all_add() {
        let d = line_diff("", "x\ny");
        assert_eq!(kinds(&d), vec!["+", "+"]);
    }

    #[test]
    fn output_is_capped() {
        let old = String::new();
        let new = "x\n".repeat(600);
        let d = line_diff(&old, &new);
        assert_eq!(d.len(), MAX_DIFF_LINES + 1);
        assert_eq!(d.last(), Some(&DiffLine::Del("…(diff 已截断)".into())));
    }
}
