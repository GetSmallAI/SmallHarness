pub fn unified_diff(old_text: &str, new_text: &str, path: &str) -> String {
    let old_lines: Vec<&str> = old_text.split('\n').collect();
    let new_lines: Vec<&str> = new_text.split('\n').collect();
    let mut out: Vec<String> = vec![format!("--- {path}"), format!("+++ {path}")];
    let (mut i, mut j) = (0usize, 0usize);
    while i < old_lines.len() || j < new_lines.len() {
        if i < old_lines.len() && j < new_lines.len() && old_lines[i] == new_lines[j] {
            i += 1;
            j += 1;
            continue;
        }
        out.push(format!("@@ -{} +{} @@", i + 1, j + 1));
        while i < old_lines.len() && (j >= new_lines.len() || old_lines[i] != new_lines[j]) {
            out.push(format!("-{}", old_lines[i]));
            i += 1;
        }
        while j < new_lines.len() && (i >= old_lines.len() || old_lines[i] != new_lines[j]) {
            out.push(format!("+{}", new_lines[j]));
            j += 1;
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_single_replacement() {
        let d = unified_diff("a\nb\nc", "a\nB\nc", "f.txt");
        assert!(d.contains("-b"));
        assert!(d.contains("+B"));
        assert!(d.contains("@@ -2 +2 @@"));
    }
}
