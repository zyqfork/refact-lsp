use std::collections::VecDeque;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use hashbrown::HashMap;
use itertools::Itertools;

use crate::call_validation::DiffChunk;
use crate::files_in_workspace::read_file_from_disk;
use crate::privacy::PrivacySettings;


#[derive(Clone, Debug)]
struct Edit {
    before_path: Option<String>,
    after_path: Option<String>,
    hunk: Vec<String>,
}

#[derive(Clone, Eq, PartialEq)]
pub enum LineType {
    Plus,
    Minus,
    Space,
}

impl fmt::Display for LineType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let printable = match self {
            LineType::Plus => "+",
            LineType::Minus => "-",
            LineType::Space => " ",
        };
        write!(f, "{}", printable)
    }
}


#[derive(Clone, Eq, PartialEq)]
pub struct DiffLine {
    pub line: String,
    pub line_type: LineType,
    pub file_line_num_idx: Option<usize>,
    pub correct_spaces_offset: Option<i64>,
}

#[derive(Clone, Eq, PartialEq)]
pub struct DiffBlock {
    pub file_name_before: PathBuf,
    pub file_name_after: PathBuf,
    pub action: String,
    pub diff_lines: Vec<DiffLine>,
    pub hunk_idx: usize,
    pub file_lines: Arc<Vec<String>>,
}

impl DiffBlock {
    #[allow(dead_code)]
    pub fn display(&self) -> String {
        let mut output = format!(
            "--- {:?}\n+++ {:?}\n@@ ... @@\n",
            &self.file_name_before,
            &self.file_name_after
        );
        for line in self.diff_lines.iter() {
            output.push_str(&format!("{}{}", line.line_type, line.line));
        }
        output
    }
}

fn process_fenced_block(lines: &[&str], start_line_num: usize) -> (usize, Vec<Edit>) {
    let mut line_num = start_line_num;
    while line_num < lines.len() {
        if lines[line_num].starts_with("```") {
            break;
        }
        line_num += 1;
    }

    let mut block: Vec<&str> = lines[start_line_num..line_num].to_vec();
    block.push("@@ @@");

    let mut before_path = None;
    let mut after_path = None;
    if block[0].starts_with("--- ") && block[1].starts_with("+++ ") {
        before_path = Some(block[0][4..].trim().to_string());
        after_path = Some(block[1][4..].trim().to_string());
        block = block[2..].to_vec();
    }

    let mut edits = Vec::new();
    let mut hunk = Vec::new();
    let add_remove_rename_block =
        before_path.as_ref().map_or(false, |x| x.starts_with("/dev/null"))
            || after_path.as_ref().map_or(false, |x| x.starts_with("/dev/null"))
            || before_path.as_ref().map_or(false, |x| after_path.as_ref().map_or(false, |y| x != y));

    for line in block {
        hunk.push(line.to_string());
        if line.len() < 2 {
            continue;
        }

        if line.starts_with("+++ ")
            && hunk.len() >= 3
            && hunk[hunk.len() - 2].starts_with("--- ") {
            let before_path_new = Some(hunk[hunk.len() - 2][4..].trim().to_string());
            if hunk[hunk.len() - 3] == "\n" {
                hunk.truncate(hunk.len() - 3);
            } else {
                hunk.truncate(hunk.len() - 2);
            }

            edits.push(Edit {
                before_path: before_path.clone(),
                after_path: after_path.clone(),
                hunk: hunk.clone(),
            });
            before_path = before_path_new;
            after_path = Some(line[4..].trim().to_string());
            hunk.clear();
            continue;
        }

        let op = line.chars().next().unwrap();
        if op == '-' || op == '+' || (add_remove_rename_block && op != '@') {
            continue;
        }
        if op != '@' {
            continue;
        }
        if hunk.len() <= 1 {
            hunk.clear();
            continue;
        }

        hunk.pop();
        edits.push(Edit {
            before_path: before_path.clone(),
            after_path: after_path.clone(),
            hunk: hunk.clone(),
        });
        hunk.clear();
    }

    (line_num + 1, edits)
}

fn get_edit_hunks(content: &str) -> Vec<Edit> {
    let lines: Vec<&str> = content.lines().collect();
    let mut line_num = 0;
    let mut edits: Vec<Edit> = vec![];

    while line_num < lines.len() {
        while line_num < lines.len() {
            let line = lines[line_num];
            if line.starts_with("```diff") {
                let (new_line_num, these_edits) = process_fenced_block(&lines, line_num + 1);
                line_num = new_line_num;
                edits.extend(these_edits);
                break;
            }
            line_num += 1;
        }
    }
    edits
}

async fn edit_hunks_to_diff_blocks(edits: &Vec<Edit>, privacy_settings: Arc<PrivacySettings>) -> Result<Vec<DiffBlock>, String> {
    fn make_add_type_diff_block(idx: usize, before_path: &PathBuf, after_path: &PathBuf, edit: &Edit) -> DiffBlock {
        let diff_lines = edit.hunk
            .iter()
            .map(|x| DiffLine {
                line: if x.starts_with("+") { x[1..].to_string() } else { x.clone() },
                line_type: LineType::Plus,
                file_line_num_idx: Some(0),
                correct_spaces_offset: Some(0),
            })
            .collect::<Vec<_>>();
        DiffBlock {
            file_name_before: before_path.clone(),
            file_name_after: after_path.clone(),
            action: "add".to_string(),
            file_lines: Arc::new(vec![]),
            hunk_idx: idx,
            diff_lines,
        }
    }

    fn make_remove_type_diff_block(idx: usize, before_path: &PathBuf, after_path: &PathBuf) -> DiffBlock {
        DiffBlock {
            file_name_before: before_path.clone(),
            file_name_after: after_path.clone(),
            action: "remove".to_string(),
            file_lines: Arc::new(vec![]),
            hunk_idx: idx,
            diff_lines: vec![],
        }
    }

    let mut diff_blocks = vec![];
    let mut files_to_filelines = HashMap::new();
    for (idx, edit) in edits.iter().enumerate() {
        let before_path = match edit.before_path.clone() {
            Some(p) => PathBuf::from(p.clone()),
            None => {
                return Err(format!("cannot get a correct 'before' file name from the diff chunk:\n{edit:?}\n"));
            }
        };
        let after_path = match edit.after_path.clone() {
            Some(p) => PathBuf::from(p.clone()),
            None => {
                return Err(format!("cannot get a correct 'after' file name from the diff chunk:\n{edit:?}\n"));
            }
        };
        let mut action = "edit".to_string();
        if edit.before_path.clone().map_or(false, |x| x == "/dev/null") {
            diff_blocks.push(make_add_type_diff_block(idx, &before_path, &after_path, edit));
            continue;
        }
        if edit.after_path.clone().map_or(false, |x| x == "/dev/null") {
            diff_blocks.push(make_remove_type_diff_block(idx, &before_path, &after_path));
            continue;
        }

        // more checks for `rename` action
        if before_path != after_path {
            action = "rename".to_string();
            if after_path.exists() {
                return Err(format!(
                    "cannot rename {before_path:?}, destination file {after_path:?} name already exists"
                ));
            }
        }

        let file_lines = files_to_filelines
            .entry(before_path.clone())
            .or_insert(Arc::new(read_file_from_disk(privacy_settings.clone(), &before_path)
                .await
                .map(
                    |x| x
                        .lines()
                        .into_iter()
                        .map(|x| {
                            if let Some(stripped_row) = x.to_string()
                                .replace("\r\n", "\n")
                                .strip_suffix("\n") {
                                stripped_row.to_string()
                            } else {
                                x.to_string()
                            }
                        })
                        .collect::<Vec<_>>()
                )?));
        let mut block_has_minus_plus = false;
        let mut current_lines = vec![];
        let has_any_line_no_leading_space = edit.hunk.iter().any(|x| !x.starts_with(" "));
        for line in edit.hunk.iter() {
            if line.starts_with("-") || line.starts_with("+") {
                let is_plus = line.starts_with("+");
                current_lines.push(DiffLine {
                    line: line[1..].to_string(),
                    line_type: if is_plus { LineType::Plus } else { LineType::Minus },
                    file_line_num_idx: None,
                    correct_spaces_offset: None,
                });
                block_has_minus_plus = true;
            } else {
                if block_has_minus_plus {
                    diff_blocks.push(DiffBlock {
                        file_name_before: before_path.clone(),
                        file_name_after: after_path.clone(),
                        action: action.clone(),
                        file_lines: file_lines.clone(),
                        hunk_idx: idx,
                        diff_lines: current_lines.clone(),
                    });
                    block_has_minus_plus = false;
                    current_lines.clear();
                }
                current_lines.push(DiffLine {
                    line: if !has_any_line_no_leading_space && line.starts_with(" ") {
                        line[1..].to_string()
                    } else {
                        line.clone()
                    },
                    line_type: LineType::Space,
                    file_line_num_idx: None,
                    correct_spaces_offset: None,
                })
            }
        }
        if !current_lines.is_empty() {
            diff_blocks.push(DiffBlock {
                file_name_before: before_path.clone(),
                file_name_after: after_path.clone(),
                action: action.clone(),
                file_lines: file_lines.clone(),
                hunk_idx: idx,
                diff_lines: current_lines.clone(),
            });
        }
    }
    Ok(diff_blocks)
}

fn search_diff_block_text_location(diff_blocks: &mut Vec<DiffBlock>) {
    for i in 0..diff_blocks.len() {
        let mut blocks_to_search = diff_blocks
            .iter_mut()
            .filter(|x| x.hunk_idx == i)
            .collect::<VecDeque<_>>();
        if blocks_to_search.is_empty() {
            continue;
        }

        let mut file_line_start_offset: usize = 0;
        while let Some(diff_block) = blocks_to_search.pop_front() {
            let mut diff_line_start_offset: usize = 0;
            while diff_line_start_offset <= diff_block.diff_lines.len() {
                let mut found = false;
                for diff_line_span_size in (1..diff_block.diff_lines.len() - diff_line_start_offset + 1).rev() {
                    let span = &diff_block.diff_lines[diff_line_start_offset..diff_line_start_offset + diff_line_span_size];
                    let diff_lines_span = span
                        .iter()
                        .map(|x| &x.line)
                        .map(|x| x.trim_start().to_string())
                        .collect::<Vec<_>>();
                    if span.iter().any(|x| x.line_type == LineType::Plus)
                        || diff_line_span_size >= diff_block.file_lines.len() {
                        continue;
                    }
                    for file_line_idx in file_line_start_offset..=diff_block.file_lines.len() - diff_line_span_size {
                        let file_lines_span = diff_block.file_lines[file_line_idx..file_line_idx + diff_line_span_size]
                            .iter()
                            .map(|x| x.trim_start().to_string())
                            .collect::<Vec<_>>();
                        if file_line_idx > file_line_start_offset &&
                            (file_lines_span.is_empty() || diff_lines_span.iter().all(|c| c == "")) {
                            continue;
                        }
                        if file_lines_span == diff_lines_span {
                            for (idx, line) in diff_block.diff_lines[diff_line_start_offset..diff_line_start_offset + diff_line_span_size]
                                .iter_mut()
                                .enumerate() {
                                let file_lines_idents_count = diff_block.file_lines[file_line_idx + idx]
                                    .chars()
                                    .take_while(|x| x.eq(&' '))
                                    .join("")
                                    .len() as i64;
                                let diff_lines_idents_count = line.line
                                    .chars()
                                    .take_while(|x| x.eq(&' '))
                                    .join("")
                                    .len() as i64;
                                line.file_line_num_idx = Some(file_line_idx + idx);
                                line.correct_spaces_offset = Some(file_lines_idents_count - diff_lines_idents_count);
                            }
                            diff_line_start_offset = diff_line_start_offset + diff_line_span_size;
                            file_line_start_offset = file_line_idx + diff_line_span_size;
                            found = true;
                            break;
                        }
                    }
                    if found {
                        break;
                    }
                }
                if !found {
                    diff_line_start_offset += 1;
                }
            }
        }
    }
}

fn splitting_diff_blocks(diff_blocks: &Vec<DiffBlock>) -> Vec<DiffBlock> {
    let mut exported_blocks = vec![];
    for (_, blocks) in &diff_blocks.iter().group_by(|x| x.hunk_idx) {
        let new_blocks: Vec<_> = blocks.cloned().collect();
        let diff_block = new_blocks.first().expect("cannot find diff block");
        if new_blocks.len() == 1 && diff_block.action == "edit" {
            if diff_block.diff_lines.iter().all(|x| x.line_type == LineType::Space) {
                let original_text = diff_block.file_lines.join("\n");
                let text_after = diff_block.diff_lines.iter().map(|x| x.line.clone()).join("\n");
                let diffs = diff::lines(&original_text, &text_after);
                let mut line_num: usize = 0;
                let mut diff_lines = vec![];
                for diff in diffs {
                    match diff {
                        diff::Result::Left(l) => {
                            diff_lines.push(DiffLine {
                                line: l.to_string(),
                                line_type: LineType::Minus,
                                file_line_num_idx: Some(line_num),
                                correct_spaces_offset: Some(0),
                            });
                            line_num += 1;
                        }
                        diff::Result::Right(r) => {
                            diff_lines.push(DiffLine {
                                line: r.to_string(),
                                line_type: LineType::Plus,
                                file_line_num_idx: Some(line_num),
                                correct_spaces_offset: Some(0),
                            });
                        }
                        diff::Result::Both(_, _) => {
                            line_num += 1;
                            if !diff_lines.is_empty() {
                                exported_blocks.push(DiffBlock {
                                    file_name_before: diff_block.file_name_before.clone(),
                                    file_name_after: diff_block.file_name_after.clone(),
                                    action: diff_block.action.clone(),
                                    file_lines: diff_block.file_lines.clone(),
                                    hunk_idx: diff_block.hunk_idx,
                                    diff_lines: diff_lines.clone(),
                                });
                                diff_lines.clear();
                            }
                        }
                    }
                }
                if !diff_lines.is_empty() {
                    exported_blocks.push(DiffBlock {
                        file_name_before: diff_block.file_name_before.clone(),
                        file_name_after: diff_block.file_name_after.clone(),
                        action: diff_block.action.clone(),
                        file_lines: diff_block.file_lines.clone(),
                        hunk_idx: diff_block.hunk_idx,
                        diff_lines: diff_lines.clone(),
                    });
                    diff_lines.clear();
                }
            } else {
                exported_blocks.extend(new_blocks);
            }
        } else {
            exported_blocks.extend(new_blocks);
        }
    }
    exported_blocks.clone()
}

// Step 0. Remove those chunks which remove and add the same code
// Step 1. Fix idents using correct_spaces_offset
// Step 2. If non-found is the first line, and it is a `+` type then set the 0 index
// Step 3. Fix missing `+` lines. If line is without `+` symbol and is file line index is not found then consider it a `+` line (except the first line)
// Step 4. Fix missing `-` lines. If line is without `-` symbol and file index is found and the nearest `+` line is quite similar then consider it as a `-` line
// Step 5. Fill out all non-found file indexes using the last one found.
fn normalize_diff_block(diff_block: &mut DiffBlock) -> Result<(), String> {
    if diff_block.diff_lines.is_empty() {
        return Ok(());
    }

    // Step 1
    for diff_line in diff_block.diff_lines.iter_mut() {
        if let Some(correct_spaces_offset) = diff_line.correct_spaces_offset {
            if correct_spaces_offset > 0 {
                diff_line.line.insert_str(0, &" ".repeat(correct_spaces_offset as usize));
            } else if correct_spaces_offset < 0 {
                diff_line.line = diff_line.line.chars().skip(correct_spaces_offset.abs() as usize).join("");
            }
        }
    }

    // Step 2
    match diff_block.diff_lines.get_mut(0) {
        Some(line) => {
            if line.line_type == LineType::Plus && line.file_line_num_idx.is_none() {
                line.file_line_num_idx = Some(0);
            }
        }
        None => {}
    };
    diff_block.diff_lines = diff_block
        .diff_lines
        .iter()
        .skip_while(|x| x.line_type == LineType::Space && x.file_line_num_idx.is_none())
        .cloned()
        .collect::<Vec<_>>();

    // Step 3 (doesn't work well enough)
    // for diff_line in diff_block.diff_lines.iter_mut() {
    //     if diff_line.line_type == LineType::Space || diff_line.file_line_num_idx.is_none() {
    //         diff_line.line_type = LineType::Plus;
    //     }
    // }

    // Step 4
    let diff_lines_copy = diff_block.diff_lines.clone();
    for (idx, diff_line) in diff_block.diff_lines.iter_mut().enumerate() {
        if diff_line.line_type == LineType::Space
            && diff_line.file_line_num_idx.is_some()
            && idx < diff_lines_copy.len() - 1 {
            let nearest_plus_diff_line = match diff_lines_copy[idx + 1..]
                .iter()
                .find(|x| x.line_type == LineType::Plus) {
                Some(item) => item,
                None => {
                    continue
                }
            };
            if diff_line.line == nearest_plus_diff_line.line {
                diff_line.line_type = LineType::Minus;
            }
        }
    }

    // Step 5
    let mut last_file_line_num_idx = None;
    for diff_line in diff_block.diff_lines.iter_mut() {
        if diff_line.file_line_num_idx.is_some() {
            last_file_line_num_idx = diff_line.file_line_num_idx.map(|x| x + 1);
        } else {
            diff_line.file_line_num_idx = last_file_line_num_idx;
        }
    }

    // Validation step
    let non_found_lines = diff_block.diff_lines
        .iter()
        .filter(|x| x.line_type != LineType::Space && x.file_line_num_idx.is_none())
        .map(|x| format!("{}{}", x.line_type, x.line))
        .collect::<Vec<_>>();
    if !non_found_lines.is_empty() {
        return Err(format!(
            "blocks of code signed with '-' weren't found in a file\n{}\n",
            non_found_lines.join("\n")
        ));
    }

    Ok(())
}

pub fn diff_blocks_to_diff_chunks(diff_blocks: &Vec<DiffBlock>) -> Vec<DiffChunk> {
    diff_blocks
        .iter()
        .filter_map(|block| {
            let useful_block_lines = block
                .diff_lines
                .iter()
                .filter(|x| x.line_type != LineType::Space)
                .collect::<Vec<_>>();
            let (filename, filename_rename) = if block.action == "add" {
                (block.file_name_after.to_string_lossy().to_string(), None)
            } else if block.action == "remove" {
                (block.file_name_before.to_string_lossy().to_string(), None)
            } else if block.action == "rename" {
                (block.file_name_before.to_string_lossy().to_string(),
                 Some(block.file_name_after.to_string_lossy().to_string()))
            } else {  // edit
                assert_eq!(block.file_name_before, block.file_name_after);
                (block.file_name_before.to_string_lossy().to_string(), None)
            };
            let lines_remove = useful_block_lines
                .iter()
                .filter(|x| x.line_type == LineType::Minus)
                .map(|x| format!("{}\n", x.line.clone()))
                .join("");
            let lines_add = useful_block_lines
                .iter()
                .filter(|x| x.line_type == LineType::Plus)
                .map(|x| format!("{}\n", x.line.clone()))
                .join("");
            if lines_remove == lines_add {
                return None;
            }
            Some(DiffChunk {
                file_name: filename,
                file_name_rename: filename_rename,
                file_action: block.action.clone(),
                line1: useful_block_lines
                    .iter()
                    .map(|x| x.file_line_num_idx.clone().expect("All file_line_num_idx must be filled to this moment in the `normalize_diff_block` func") + 1)
                    .min()
                    .unwrap_or(1),
                line2: useful_block_lines
                    .iter()
                    .map(|x| {
                        if x.line_type == LineType::Plus {
                            x.file_line_num_idx.clone().expect("All file_line_num_idx must be filled to this moment in the `normalize_diff_block` func") + 1
                        } else {
                            x.file_line_num_idx.clone().expect("All file_line_num_idx must be filled to this moment in the `normalize_diff_block` func") + 2
                        }
                    })
                    .max()
                    .unwrap_or(1),
                lines_remove,
                lines_add,
                ..Default::default()
            })
        })
        .collect()
}


pub struct UnifiedDiffFormat {}

impl UnifiedDiffFormat {
    pub fn prompt(
        workspace_projects_dirs: Vec<String>
    ) -> String {
        assert_eq!(workspace_projects_dirs.is_empty(), false);
        let prompt = r#"YOU ARE THE WORLD'S LEADING AUTO CODING ASSISTANT.
You will receive a file containing code, along with one or several modified sections.
Your task is to generate a unified diff in a specified format, comparing the original file to the updated portion.
In the diff generation use following project directory:
%WORKSPACE_PROJECTS_DIRS%

### UNIFIED DIFF FORMATTING RULES

## Rules to generate correct diffs:
- Fence the diff with "```diff" and "```".
- Make changes for every given file.
- Return edits similar to unified diffs that `diff -U2` would produce.
- Don't include line numbers like `diff -U2` does. The user's patch tool doesn't need them.
- Copy a few lines from the original file and paste them before the `-` and `+` lines, otherwise the diff will be incorrect.
- Don't include timestamps with the file paths.
- `@@ ... block @@` hunk MUST contain `-` or `+` types of lines.
- The user's patch tool needs CORRECT patches that apply cleanly against the current contents of the file.
- When using the `@@ ... block @@`, make sure you mark all new or modified lines with `+`.
- When using the `@@ ... block @@`, make sure you include and mark all lines that need to be removed or changed as `-` lines.
- Output hunks in whatever order makes the most sense.
- Rewrite the whole blocks of code instead of making multiple small changes.
- Use filenames from the user as given, don't change them.
- Include / import symbols which are used in the diff.
- When editing a function, method, loop, etc. use a hunk to replace the *entire* code block.
- When using the `@@ ... @@`, delete the entire existing version with `-` lines and then add a new, updated version with `+` lines. This will help you generate correct code and correct diffs.
- When using the `@@ ... @@`, only output hunks that specify changes with `+` or `-` lines.
- When using the `@@ ... @@`, copy a few lines from the original file and paste them before the `-` and `+` lines.
- @@ ... @@ format example for the task: "Replace is_prime with a call to sympy"
```diff
--- %FIRST_WORKSPACE_PROJECT_DIR%/test.py
+++ %FIRST_WORKSPACE_PROJECT_DIR%/test.py
@@ ... @@
+import sympy
+
@@ ... @@
-def is_prime(x):
-    if x < 2:
-        return False
-    for i in range(2,
-                  int(math.sqrt(x)) + 1):
-        if x % i == 0:
-            return False
-    return True
@@ ... @@
-@app.route('/prime/<int:n>')
-def nth_prime(n):
-    count = 0
-    num = 1
-    while count < n:
-        num += 1
-        if is_prime(num):
-            count += 1
-    return str(num)
+@app.route('/prime/<int:n>')
+def nth_prime(n):
+    count = 0
+    num = 1
+    while count < n:
+        num += 1
+        if sympy.isprime(num):
+            count += 1
+    return str(num)
@@ ... @@
+
+def nth_prime_test(n):
+    pass
```

USING `+` and `-` markings IS MANDATORY!!!
DO NOT FORGET TO FOLLOW THE REULES AND USE UNIFIED DIFF FORMAT ONLY!"#.to_string();
        prompt
            .replace("%WORKSPACE_PROJECTS_DIRS%", &workspace_projects_dirs.join("\n"))
            .replace("%FIRST_WORKSPACE_PROJECT_DIR%", &workspace_projects_dirs[0])
    }

    pub async fn parse_message(
        content: &str,
        privacy_settings: Arc<PrivacySettings>,
    ) -> Result<Vec<DiffChunk>, String> {
        let edits = get_edit_hunks(content);
        let mut diff_blocks = edit_hunks_to_diff_blocks(&edits, privacy_settings).await?;
        search_diff_block_text_location(&mut diff_blocks);
        let mut diff_blocks = splitting_diff_blocks(&diff_blocks);
        for block in diff_blocks.iter_mut() {
            match normalize_diff_block(block) {
                Ok(_) => {}
                Err(err) => {
                    return Err(err);
                }
            };
        }
        let filtered_blocks = diff_blocks
            .into_iter()
            .filter(|x| if x.action == "edit" {
                x.diff_lines
                    .iter()
                    .any(|x| x.line_type == LineType::Plus || x.line_type == LineType::Minus)
            } else { true })
            .collect::<Vec<_>>();
        let chunks = diff_blocks_to_diff_chunks(&filtered_blocks)
            .into_iter()
            .unique()
            .collect::<Vec<_>>();
        Ok(chunks)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use itertools::Itertools;

    use crate::privacy::PrivacySettings;
    use crate::tools::patch::unified_diff_format::UnifiedDiffFormat;
    use crate::call_validation::DiffChunk;
    use crate::diffs::{apply_diff_chunks_to_text, unwrap_diff_apply_outputs};

    fn apply_diff(path: &String, chunks: &Vec<DiffChunk>) -> (String, String) {
        let text = std::fs::read_to_string(PathBuf::from(path)).unwrap();
        let (results, outputs) = apply_diff_chunks_to_text(
            &text,
            chunks.iter().enumerate().collect::<Vec<_>>(),
            vec![],
            1,
        );
        let outputs_unwrapped = unwrap_diff_apply_outputs(outputs, chunks.clone());
        assert_eq!(outputs_unwrapped.into_iter().all(|x| x.applied), true);
        let changed_text = results[0].clone().file_text.unwrap();
        (text, changed_text)
    }

    #[tokio::test]
    async fn test_empty_1() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/frog.py
+++ tests/emergency_frog_situation/frog.py
@@ ... @@
```
Another text"#;
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_empty_2() {
        let input = r#""#;

        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_empty_3() {
        let input = r#"Initial text
```diff
Another text"#;

        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_empty_4() {
        let input = r#"Initial text
```"#;

        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_empty_5() {
        let input = r#"Initial text
```diff
some invalid text
```
```
```diff"#;

        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_empty_6() {
        let input = r#"Initial text
```diff
+++
```
Another text"#;

        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().starts_with("cannot get a correct 'before' file name from the diff chunk:"));
    }

    #[tokio::test]
    #[ignore]
    async fn test_empty_7() {
        let input = r#"Initial text
```diff
---
+++
@@ ... @@
```
Another text"#;

        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    #[ignore]
    async fn test_empty_8() {
        let input = r#"Initial text
```diff
---
+++ asd
@@ ... @@
```
Another text"#;

        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_simple_hunk_1() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/frog.py
+++ tests/emergency_frog_situation/frog.py
@@ ... @@
-class Frog:
+class AnotherFrog:
```
Another text"#;
        let gt_changed_text = r#"import numpy as np

DT = 0.01

class AnotherFrog:
    def __init__(self, x, y, vx, vy):"#;


        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/frog.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 5,
                line2: 6,
                lines_remove: "class Frog:\n".to_string(),
                lines_add: "class AnotherFrog:\n".to_string(),
                ..Default::default()
            }
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        let (_, changed_text) = apply_diff(
            &"./tests/emergency_frog_situation/frog.py".to_string(),
            &result,
        );
        let cropped_text = changed_text.lines().take(6).join("\n");

        assert_eq!(result, gt_result);
        assert_eq!(cropped_text, gt_changed_text);
    }

    #[tokio::test]
    async fn test_simple_hunk_2() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/frog.py
+++ tests/emergency_frog_situation/frog.py
@@ ... @@
 DT = 0.01


-class Frog:
```
Another text"#;
        let gt_changed_text = r#"import numpy as np

DT = 0.01

    def __init__(self, x, y, vx, vy):"#;



        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/frog.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 5,
                line2: 6,
                lines_remove: "class Frog:\n".to_string(),
                lines_add: "".to_string(),
                ..Default::default()
            }
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        assert_eq!(result, gt_result);

        let (_, changed_text) = apply_diff(
            &"./tests/emergency_frog_situation/frog.py".to_string(),
            &result,
        );
        let cropped_text = changed_text.lines().take(5).join("\n");
        assert_eq!(cropped_text, gt_changed_text);
    }

    #[tokio::test]
    async fn test_simple_hunk_3() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/frog.py
+++ tests/emergency_frog_situation/frog.py
@@ ... @@
 DT = 0.01

 class Frog:
+    # Frog class description
```
Another text"#;
        let gt_changed_text = r#"import numpy as np

DT = 0.01

class Frog:
    # Frog class description
    def __init__(self, x, y, vx, vy):"#;



        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/frog.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 6,
                line2: 6,
                lines_remove: "".to_string(),
                lines_add: "    # Frog class description\n".to_string(),
                ..Default::default()
            }
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        let (_, changed_text) = apply_diff(
            &"./tests/emergency_frog_situation/frog.py".to_string(),
            &result,
        );
        let cropped_text = changed_text.lines().take(7).join("\n");

        assert_eq!(result, gt_result);
        assert_eq!(cropped_text, gt_changed_text);
    }

    #[tokio::test]
    async fn test_simple_hunk_4() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/frog.py
+++ tests/emergency_frog_situation/frog.py
@@ ... @@
+    # Frog class description
```
Another text"#;
        let gt_changed_text = r#"    # Frog class description
import numpy as np

DT = 0.01"#;



        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/frog.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 1,
                line2: 1,
                lines_remove: "".to_string(),
                lines_add: "    # Frog class description\n".to_string(),
                ..Default::default()
            }
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        let (_, changed_text) = apply_diff(
            &"./tests/emergency_frog_situation/frog.py".to_string(),
            &result,
        );
        let cropped_text = changed_text.lines().take(4).join("\n");

        assert_eq!(result, gt_result);
        assert_eq!(cropped_text, gt_changed_text);
    }

    #[tokio::test]
    #[ignore]
    async fn test_simple_hunk_5() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/frog.py
+++ tests/emergency_frog_situation/frog.py
@@ ... @@
-    def jump(self, pond_width, pond_height):
```
Another text"#;
        let gt_changed_text = r#"import numpy as np

DT = 0.01

class Frog:
    def __init__(self, x, y, vx, vy):
        self.x = x
        self.y = y
        self.vx = vx
        self.vy = vy

    def bounce_off_banks(self, pond_width, pond_height):
        if self.x < 0:
            self.vx = np.abs(self.vx)
        elif self.x > pond_width:
            self.vx = -np.abs(self.vx)
        if self.y < 0:
            self.vy = np.abs(self.vy)
        elif self.y > pond_height:
            self.vy = -np.abs(self.vy)

        self.x += self.vx * DT
        self.y += self.vy * DT"#;



        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/frog.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 22,
                line2: 23,
                lines_remove: "    def jump(self, pond_width, pond_height):\n".to_string(),
                lines_add: "".to_string(),
                ..Default::default()
            }
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        let (_, changed_text) = apply_diff(
            &"./tests/emergency_frog_situation/frog.py".to_string(),
            &result,
        );
        let cropped_text = changed_text.lines().take(23).join("\n");

        assert_eq!(result, gt_result);
        assert_eq!(cropped_text, gt_changed_text);
    }

    #[tokio::test]
    #[ignore]
    async fn test_simple_hunk_6() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/frog.py
+++ tests/emergency_frog_situation/frog.py
@@ ... @@

-    def jump(self, pond_width, pond_height):
```
Another text"#;
        let gt_changed_text = r#"import numpy as np

DT = 0.01

class Frog:
    def __init__(self, x, y, vx, vy):
        self.x = x
        self.y = y
        self.vx = vx
        self.vy = vy

    def bounce_off_banks(self, pond_width, pond_height):
        if self.x < 0:
            self.vx = np.abs(self.vx)
        elif self.x > pond_width:
            self.vx = -np.abs(self.vx)
        if self.y < 0:
            self.vy = np.abs(self.vy)
        elif self.y > pond_height:
            self.vy = -np.abs(self.vy)

        self.x += self.vx * DT
        self.y += self.vy * DT"#;



        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/frog.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 22,
                line2: 23,
                lines_remove: "    def jump(self, pond_width, pond_height):\n".to_string(),
                lines_add: "".to_string(),
                ..Default::default()
            }
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        let (_, changed_text) = apply_diff(
            &"./tests/emergency_frog_situation/frog.py".to_string(),
            &result,
        );
        let cropped_text = changed_text.lines().take(23).join("\n");

        assert_eq!(result, gt_result);
        assert_eq!(cropped_text, gt_changed_text);
    }

    #[tokio::test]
    #[ignore]
    async fn test_complex_hunk_1() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/frog.py
+++ tests/emergency_frog_situation/frog.py
@@ ... @@
     def bounce_off_banks(self, pond_width, pond_height):
         if self.x < 0:
             self.vx = np.abs(self.vx)
-        elif self.x > pond_width:
+        # test1:
+        elif self.x > pond:
-            self.vx = -np.abs(self.vx)
-        if self.y < 0:
+            # what is that?
+            pass
+        if self.y > 0:
             self.vy = np.abs(self.vy)
         elif self.y > pond_height:
-            self.vy = -np.abs(self.vy)
+            self.vx = -np.abs(self.vy)
```
Another text"#;
        let gt_changed_text = r#"import numpy as np

DT = 0.01

class Frog:
    def __init__(self, x, y, vx, vy):
        self.x = x
        self.y = y
        self.vx = vx
        self.vy = vy

    def bounce_off_banks(self, pond_width, pond_height):
        if self.x < 0:
            self.vx = np.abs(self.vx)
        # test1:
        elif self.x > pond:
            # what is that?
            pass
        if self.y > 0:
            self.vy = np.abs(self.vy)
        elif self.y > pond_height:
            self.vx = -np.abs(self.vy)"#;



        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/frog.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 15,
                line2: 18,
                lines_remove: "        elif self.x > pond_width:\n            self.vx = -np.abs(self.vx)\n        if self.y < 0:\n".to_string(),
                lines_add: "        # test1:\n        elif self.x > pond:\n            # what is that?\n            pass\n        if self.y > 0:\n".to_string(),
                ..Default::default()
            },
            DiffChunk {
                file_name: "tests/emergency_frog_situation/frog.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 20,
                line2: 21,
                lines_remove: "            self.vy = -np.abs(self.vy)\n".to_string(),
                lines_add: "            self.vx = -np.abs(self.vy)\n".to_string(),
                ..Default::default()
            },
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        let (_, changed_text) = apply_diff(
            &"./tests/emergency_frog_situation/frog.py".to_string(),
            &result,
        );
        let cropped_text = changed_text.lines().take(22).join("\n");

        assert_eq!(result, gt_result); // this one does not pass
        assert_eq!(cropped_text, gt_changed_text);
    }

    #[tokio::test]
    #[ignore]
    async fn test_complex_hunk_2() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/frog.py
+++ tests/emergency_frog_situation/frog.py
@@ ... @@
     def bounce_off_banks(self, pond_width, pond_height):
         if self.x < 0:
             self.vx = np.abs(self.vx)
-        elif self.x > pond_width:
+        # test1:
+        elif self.x > pond:
-            self.vx = -np.abs(self.vx)
-        if self.y < 0:
+            # what is that?
+            pass
+        if self.y > 0:
--- tests/emergency_frog_situation/frog.py
+++ tests/emergency_frog_situation/frog.py
@@ ... @@
-            self.vy = -np.abs(self.vy)
+            self.vx = -np.abs(self.vy)
```
Another text"#;



        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/frog.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 15,
                line2: 18,
                lines_remove: "        elif self.x > pond_width:\n            self.vx = -np.abs(self.vx)\n        if self.y < 0:\n".to_string(),
                lines_add: "        # test1:\n        elif self.x > pond:\n            # what is that?\n            pass\n        if self.y > 0:\n".to_string(),
                ..Default::default()
            },
            DiffChunk {
                file_name: "tests/emergency_frog_situation/frog.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 20,
                line2: 21,
                lines_remove: "            self.vy = -np.abs(self.vy)\n".to_string(),
                lines_add: "            self.vx = -np.abs(self.vy)\n".to_string(),
                ..Default::default()
            },
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        assert_eq!(result, gt_result);
    }

    #[tokio::test]
    #[ignore]
    async fn test_complex_hunk_3() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/frog.py
+++ tests/emergency_frog_situation/frog.py
@@ ... @@
     def bounce_off_banks(self, pond_width, pond_height):
         if self.x < 0:
             self.vx = np.abs(self.vx)
-        elif self.x > pond_width:
+        # test1:
+        elif self.x > pond:
-            self.vx = -np.abs(self.vx)
-        if self.y < 0:
+            # what is that?
+            pass
+        if self.y > 0:
```
```diff
--- tests/emergency_frog_situation/frog.py
+++ tests/emergency_frog_situation/frog.py
@@ ... @@
-            self.vy = -np.abs(self.vy)
+            self.vx = -np.abs(self.vy)
```
Another text"#;



        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/frog.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 15,
                line2: 18,
                lines_remove: "        elif self.x > pond_width:\n            self.vx = -np.abs(self.vx)\n        if self.y < 0:\n".to_string(),
                lines_add: "        # test1:\n        elif self.x > pond:\n            # what is that?\n            pass\n        if self.y > 0:\n".to_string(),
                ..Default::default()
            },
            DiffChunk {
                file_name: "tests/emergency_frog_situation/frog.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 20,
                line2: 21,
                lines_remove: "            self.vy = -np.abs(self.vy)\n".to_string(),
                lines_add: "            self.vx = -np.abs(self.vy)\n".to_string(),
                ..Default::default()
            },
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        assert_eq!(result, gt_result);
    }

    #[tokio::test]
    #[ignore]
    async fn test_complex_hunk_4() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/frog.py
+++ tests/emergency_frog_situation/frog.py
@@ ... @@
         self.y = np.clip(self.y, 0, pond_height)
+        # extra row 1
+        # extra row 2
+        # extra row 3
--- tests/emergency_frog_situation/frog.py
+++ tests/emergency_frog_situation/frog.py
@@ ... @@
-import numpy as np
+import numpy as np
+# extra row 1
+# extra row 2
+# extra row 3
```
Another text"#;
        let gt_changed_text = r#"import numpy as np
# extra row 1
# extra row 2
# extra row 3

DT = 0.01

class Frog:
    def __init__(self, x, y, vx, vy):
        self.x = x
        self.y = y
        self.vx = vx
        self.vy = vy

    def bounce_off_banks(self, pond_width, pond_height):
        if self.x < 0:
            self.vx = np.abs(self.vx)
        elif self.x > pond_width:
            self.vx = -np.abs(self.vx)
        if self.y < 0:
            self.vy = np.abs(self.vy)
        elif self.y > pond_height:
            self.vy = -np.abs(self.vy)

    def jump(self, pond_width, pond_height):
        self.x += self.vx * DT
        self.y += self.vy * DT
        self.bounce_off_banks(pond_width, pond_height)
        self.x = np.clip(self.x, 0, pond_width)
        self.y = np.clip(self.y, 0, pond_height)
        # extra row 1
        # extra row 2
        # extra row 3
"#;



        #[allow(unused_mut)]
        let mut gt_changed_text = String::from(gt_changed_text);
        #[cfg(target_os = "windows")]
        {
            gt_changed_text = gt_changed_text.replace("\n", "\r\n");
        }

        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/frog.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 28,
                line2: 28,
                lines_remove: "".to_string(),
                lines_add: "        # extra row 1\n        # extra row 2\n        # extra row 3\n".to_string(),
                ..Default::default()
            },
            DiffChunk {
                file_name: "tests/emergency_frog_situation/frog.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 1,
                line2: 2,
                lines_remove: "import numpy as np\n".to_string(),
                lines_add: "import numpy as np\n# extra row 1\n# extra row 2\n# extra row 3\n".to_string(),
                ..Default::default()
            },
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        let (_, changed_text) = apply_diff(
            &"./tests/emergency_frog_situation/frog.py".to_string(),
            &result,
        );

        assert_eq!(result, gt_result); // this one does not pass
        assert_eq!(changed_text, gt_changed_text);
    }

    #[tokio::test]
    #[ignore]
    async fn test_ambiguous_hunk_1() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/set_as_avatar.py
+++ tests/emergency_frog_situation/set_as_avatar.py
@@ ... @@
     """

     def __init__(self, x, y, vx, vy):
+        # extra row 1
+        # extra row 2
+        # extra row 3
```
Another text"#;
        let gt_changed_text = r#"# Picking up context, goal in this file:
# - goto parent class, two times
# - dump parent class

import frog

X,Y = 50, 50
W = 100
H = 100


# This this a comment for the Toad class, above the class
class Toad(frog.Frog):
    def __init__(self, x, y, vx, vy):
        super().__init__(x, y, vx, vy)
        self.name = "Bob"


class EuropeanCommonToad(frog.Frog):
    """
    This is a comment for EuropeanCommonToad class, inside the class
    """

    def __init__(self, x, y, vx, vy):
        # extra row 1
        # extra row 2
        # extra row 3
        super().__init__(x, y, vx, vy)
        self.name = "EU Toad""#;



        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/set_as_avatar.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 25,
                line2: 25,
                lines_remove: "".to_string(),
                lines_add: "        # extra row 1\n        # extra row 2\n        # extra row 3\n".to_string(),
                ..Default::default()
            },
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        let (_, changed_text) = apply_diff(
            &"./tests/emergency_frog_situation/set_as_avatar.py".to_string(),
            &result,
        );
        let cropped_text = changed_text.lines().take(29).join("\n");

        assert_eq!(result, gt_result); // this one does not pass
        assert_eq!(cropped_text, gt_changed_text);
    }

    #[tokio::test]
    async fn test_ambiguous_hunk_2() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/holiday.py
+++ tests/emergency_frog_situation/holiday.py
@@ ... @@
     frog2.jump()

-    # Third jump
+    # New Comment
```
Another text"#;
        let gt_changed_text = r#"import frog


if __name__ == __main__:
    frog1 = frog.Frog()
    frog2 = frog.Frog()

    # First jump
    frog1.jump()
    frog2.jump()

    # Second jump
    frog1.jump()
    frog2.jump()

    # New Comment
    frog1.jump()
    frog2.jump()

    # Forth jump
    frog1.jump()
    frog2.jump()
"#;



        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/holiday.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 16,
                line2: 17,
                lines_remove: "    # Third jump\n".to_string(),
                lines_add: "    # New Comment\n".to_string(),
                ..Default::default()
            },
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        let (_, changed_text) = apply_diff(
            &"./tests/emergency_frog_situation/holiday.py".to_string(),
            &result,
        );

        assert_eq!(result, gt_result);
        assert_eq!(changed_text, gt_changed_text);
    }

    #[tokio::test]
    async fn test_ambiguous_hunk_3() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/holiday.py
+++ tests/emergency_frog_situation/holiday.py
@@ ... @@
     frog1.jump()
     frog2.jump()

     # Second jump
+    frog3 = Frog()
     frog1.jump()
     frog2.jump()
+    frog3.jump()

-    # Third jump
+    # Third extra jump
     frog1.jump()
-    frog2.jump()
+    frog2.jump()
+    frog3.jump()
```
Another text"#;
        let gt_changed_text = r#"import frog


if __name__ == __main__:
    frog1 = frog.Frog()
    frog2 = frog.Frog()

    # First jump
    frog1.jump()
    frog2.jump()

    # Second jump
    frog3 = Frog()
    frog1.jump()
    frog2.jump()
    frog3.jump()

    # Third extra jump
    frog1.jump()
    frog2.jump()
    frog3.jump()

    # Forth jump
    frog1.jump()
    frog2.jump()
"#;



        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/holiday.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 13,
                line2: 13,
                lines_remove: "".to_string(),
                lines_add: "    frog3 = Frog()\n".to_string(),
                ..Default::default()
            },
            DiffChunk {
                file_name: "tests/emergency_frog_situation/holiday.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 15,
                line2: 15,
                lines_remove: "".to_string(),
                lines_add: "    frog3.jump()\n".to_string(),
                ..Default::default()
            },
            DiffChunk {
                file_name: "tests/emergency_frog_situation/holiday.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 16,
                line2: 17,
                lines_remove: "    # Third jump\n".to_string(),
                lines_add: "    # Third extra jump\n".to_string(),
                ..Default::default()
            },
            DiffChunk {
                file_name: "tests/emergency_frog_situation/holiday.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 18,
                line2: 19,
                lines_remove: "    frog2.jump()\n".to_string(),
                lines_add: "    frog2.jump()\n    frog3.jump()\n".to_string(),
                ..Default::default()
            },
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        assert_eq!(result, gt_result);

        let (_, changed_text) = apply_diff(
            &"./tests/emergency_frog_situation/holiday.py".to_string(),
            &result,
        );
        assert_eq!(changed_text, gt_changed_text);
    }

    #[tokio::test]
    async fn test_ambiguous_hunk_4() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/holiday.py
+++ tests/emergency_frog_situation/holiday.py
@@ ... @@
     frog1.jump()
     frog2.jump()
-    # Third jump
+    # Third extra jump
     frog1.jump()
-    frog2.jump()
+    frog2.jump()
+    frog3.jump()
```
Another text"#;
        let gt_changed_text = r#"import frog


if __name__ == __main__:
    frog1 = frog.Frog()
    frog2 = frog.Frog()

    # First jump
    frog1.jump()
    frog2.jump()

    # Second jump
    frog1.jump()
    frog2.jump()

    # Third extra jump
    frog1.jump()
    frog2.jump()
    frog3.jump()

    # Forth jump
    frog1.jump()
    frog2.jump()
"#;



        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/holiday.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 16,
                line2: 17,
                lines_remove: "    # Third jump\n".to_string(),
                lines_add: "    # Third extra jump\n".to_string(),
                ..Default::default()
            },
            DiffChunk {
                file_name: "tests/emergency_frog_situation/holiday.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 18,
                line2: 19,
                lines_remove: "    frog2.jump()\n".to_string(),
                lines_add: "    frog2.jump()\n    frog3.jump()\n".to_string(),
                ..Default::default()
            },
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        let (_, changed_text) = apply_diff(
            &"./tests/emergency_frog_situation/holiday.py".to_string(),
            &result,
        );

        assert_eq!(result, gt_result);
        assert_eq!(changed_text, gt_changed_text);
    }

    #[tokio::test]
    async fn test_ambiguous_hunk_5() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/holiday.py
+++ tests/emergency_frog_situation/holiday.py
@@ ... @@
    frog1.jump()
    frog2.jump()
+    # Third extra jump
```
Another text"#;
        let gt_changed_text = r#"import frog


if __name__ == __main__:
    frog1 = frog.Frog()
    frog2 = frog.Frog()

    # First jump
    frog1.jump()
    frog2.jump()
    # Third extra jump

    # Second jump
    frog1.jump()
    frog2.jump()

    # Third jump
    frog1.jump()
    frog2.jump()

    # Forth jump
    frog1.jump()
    frog2.jump()
"#;



        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/holiday.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 11,
                line2: 11,
                lines_remove: "".to_string(),
                lines_add: "    # Third extra jump\n".to_string(),
                ..Default::default()
            },
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        let (_, changed_text) = apply_diff(
            &"./tests/emergency_frog_situation/holiday.py".to_string(),
            &result,
        );

        assert_eq!(result, gt_result);
        assert_eq!(changed_text, gt_changed_text);
    }

    #[tokio::test]
    async fn test_ambiguous_hunk_6() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/holiday.py
+++ tests/emergency_frog_situation/holiday.py
@@ ... @@
    invalid row
    frog2.jump()
+    # Third extra jump
```
Another text"#;
        let gt_changed_text = r#"import frog


if __name__ == __main__:
    frog1 = frog.Frog()
    frog2 = frog.Frog()

    # First jump
    frog1.jump()
    frog2.jump()
    # Third extra jump

    # Second jump
    frog1.jump()
    frog2.jump()

    # Third jump
    frog1.jump()
    frog2.jump()

    # Forth jump
    frog1.jump()
    frog2.jump()
"#;



        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/holiday.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 11,
                line2: 11,
                lines_remove: "".to_string(),
                lines_add: "    # Third extra jump\n".to_string(),
                ..Default::default()
            },
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        assert_eq!(result, gt_result);

        let (_, changed_text) = apply_diff(
            &"./tests/emergency_frog_situation/holiday.py".to_string(),
            &result,
        );
        assert_eq!(changed_text, gt_changed_text);
    }

    #[tokio::test]
    async fn test_ambiguous_hunk_7() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/holiday.py
+++ tests/emergency_frog_situation/holiday.py
@@ ... @@
    invalid row
-    frog2.jump()
+    # Third extra jump
```
Another text"#;
        let gt_changed_text = r#"import frog


if __name__ == __main__:
    frog1 = frog.Frog()
    frog2 = frog.Frog()

    # First jump
    frog1.jump()
    # Third extra jump

    # Second jump
    frog1.jump()
    frog2.jump()

    # Third jump
    frog1.jump()
    frog2.jump()

    # Forth jump
    frog1.jump()
    frog2.jump()
"#;



        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/holiday.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 10,
                line2: 11,
                lines_remove: "    frog2.jump()\n".to_string(),
                lines_add: "    # Third extra jump\n".to_string(),
                ..Default::default()
            },
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        assert_eq!(result, gt_result);

        let (_, changed_text) = apply_diff(
            &"./tests/emergency_frog_situation/holiday.py".to_string(),
            &result,
        );
        assert_eq!(changed_text, gt_changed_text);
    }

    #[tokio::test]
    async fn test_ambiguous_hunk_8() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/holiday.py
+++ tests/emergency_frog_situation/holiday.py
@@ ... @@
    frog1 = frog.Frog()
    frog2 = frog.Frog()
    frog2.jump()
+    # Third extra jump
```
Another text"#;
        let gt_changed_text = r#"import frog


if __name__ == __main__:
    frog1 = frog.Frog()
    frog2 = frog.Frog()

    # First jump
    frog1.jump()
    frog2.jump()
    # Third extra jump

    # Second jump
    frog1.jump()
    frog2.jump()

    # Third jump
    frog1.jump()
    frog2.jump()

    # Forth jump
    frog1.jump()
    frog2.jump()
"#;



        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/holiday.py".to_string(),
                file_name_rename: None,
                file_action: "edit".to_string(),
                line1: 11,
                line2: 11,
                lines_remove: "".to_string(),
                lines_add: "    # Third extra jump\n".to_string(),
                ..Default::default()
            },
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        let (_, changed_text) = apply_diff(
            &"./tests/emergency_frog_situation/holiday.py".to_string(),
            &result,
        );

        assert_eq!(result, gt_result);
        assert_eq!(changed_text, gt_changed_text);
    }

    #[tokio::test]
    async fn test_add_file() {
        let input = r#"Initial text
```diff
--- /dev/null
+++ tests/emergency_frog_situation/new_file.py
@@ ... @@
+frog1 = frog.Frog()
+frog2 = frog.Frog()
```
Another text"#;



        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/new_file.py".to_string(),
                file_name_rename: None,
                file_action: "add".to_string(),
                line1: 1,
                line2: 1,
                lines_remove: "".to_string(),
                lines_add: "frog1 = frog.Frog()\nfrog2 = frog.Frog()\n".to_string(),
                is_file: false
            },
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        assert_eq!(result, gt_result);
    }

    #[tokio::test]
    async fn test_add_file_without_signs() {
        let input = r#"Initial text
```diff
--- /dev/null
+++ tests/emergency_frog_situation/new_file.py
@@ ... @@
frog1 = frog.Frog()
frog2 = frog.Frog()
```
Another text"#;



        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/new_file.py".to_string(),
                file_name_rename: None,
                file_action: "add".to_string(),
                line1: 1,
                line2: 1,
                lines_remove: "".to_string(),
                lines_add: "frog1 = frog.Frog()\nfrog2 = frog.Frog()\n".to_string(),
                is_file: false
            },
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        assert_eq!(result, gt_result);
    }

    #[tokio::test]
    #[ignore]
    async fn test_remove_file() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/holiday.py
+++ /dev/null
@@ ... @@
import frog


if __name__ == __main__:
    frog1 = frog.Frog()
```
Another text"#;



        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/holiday.py".to_string(),
                file_name_rename: None,
                file_action: "remove".to_string(),
                line1: 1,
                line2: 1,
                lines_remove: "".to_string(),
                lines_add: "".to_string(),
                is_file: true
            },
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        assert_eq!(result, gt_result);
    }

    #[tokio::test]
    #[ignore]
    async fn test_remove_file_without_signs() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/holiday.py
+++ /dev/null
@@ ... @@
<file_content>
```
Another text"#;



        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/holiday.py".to_string(),
                file_name_rename: None,
                file_action: "remove".to_string(),
                line1: 1,
                line2: 1,
                lines_remove: "".to_string(),
                lines_add: "".to_string(),
                is_file: true
            },
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        assert_eq!(result, gt_result);
    }

    #[tokio::test]
    #[ignore]
    async fn test_rename_file() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/holiday.py
+++ tests/emergency_frog_situation/new_holiday.py
@@ ... @@
<file_content>
```
Another text"#;



        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/holiday.py".to_string(),
                file_name_rename: Some("tests/emergency_frog_situation/new_holiday.py".to_string()),
                file_action: "rename".to_string(),
                line1: 1,
                line2: 1,
                lines_remove: "".to_string(),
                lines_add: "".to_string(),
                is_file: true
            },
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        assert_eq!(result, gt_result);
    }

    #[tokio::test]
    #[ignore]
    async fn test_rename_and_edit_file() {
        let input = r#"Initial text
```diff
--- tests/emergency_frog_situation/holiday.py
+++ tests/emergency_frog_situation/new_holiday.py
@@ ... @@
    frog1 = frog.Frog()
    frog2 = frog.Frog()
    frog2.jump()
+    # Third extra jump
```
Another text"#;
        let gt_changed_text = r#"import frog


if __name__ == __main__:
    frog1 = frog.Frog()
    frog2 = frog.Frog()

    # First jump
    frog1.jump()
    frog2.jump()
    # Third extra jump

    # Second jump
    frog1.jump()
    frog2.jump()

    # Third jump
    frog1.jump()
    frog2.jump()

    # Forth jump
    frog1.jump()
    frog2.jump()
"#;

        let gt_result = vec![
            DiffChunk {
                file_name: "tests/emergency_frog_situation/holiday.py".to_string(),
                file_name_rename: Some("tests/emergency_frog_situation/new_holiday.py".to_string()),
                file_action: "rename".to_string(),
                line1: 11,
                line2: 11,
                lines_remove: "".to_string(),
                lines_add: "    # Third extra jump\n".to_string(),
                is_file: true
            },
        ];
        let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
            "Failed to parse diff message"
        );
        assert_eq!(result, gt_result);

        // let (_, changed_text) = apply_diff(
        //     &"./tests/emergency_frog_situation/holiday.py".to_string(),
        //     &result,
        // );
        // assert_eq!(changed_text, gt_changed_text);
    }
    
// COMMENTED: REASON: INVALID PATH /home/svakhreev/tmp/flappy_bird/game.js"
//     #[tokio::test]
//     #[ignore]
//     async fn info_test() {
//         let input = r#"
// 
// 1. Fixing the missing curly braces in `startObstacleCreation` and `drawObstacles`.
// 2. Correcting the array initialization for `obstacles`.
// 3. Fixing the typo in `createObstacle`.
// 4. Completing the condition in `updateObstacles`.
// 5. Adding missing closing braces for functions.
// 
// Now, let's translate these changes into a unified diff format.
// 
// ```diff
// --- /home/svakhreev/tmp/flappy_bird/game.js
// +++ /home/svakhreev/tmp/flappy_bird/game.js
// @@ file_replace_block @@
// let obstacleInterval; // Variable to hold the interval ID
// 
// function startObstacleCreation() {
//     if (!obstacleInterval) { // Check if the interval is not already set
//         obstacleInterval = setInterval(createObstacle, 3000); // Create a new obstacle every 3 seconds
//     }
// }
// const obstacles = []; // Array to hold obstacles
// const obstacleWidth = 20; // Width of the obstacles
// const obstacleHeight = 100; // Height of the obstacles
// let score = 0; // Initialize score
// 
// function createObstacle() {
//     const obstacle = {
//         x: canvas.width,
//         y: Math.random() * (canvas.height - obstacleHeight),
//         width: obstacleWidth,
//         height: obstacleHeight
//     };
//     obstacles.push(obstacle);
// }
// 
// function drawObstacles() {
//     ctx.fillStyle = 'red'; // Color of the obstacles
//     obstacles.forEach(obstacle => {
//         ctx.fillRect(obstacle.x, obstacle.y, obstacle.width, obstacle.height);
//     });
// }
// 
// function updateObstacles() {
//     obstacles.forEach(obstacle => {
//         obstacle.x -= 2; // Move obstacles to the left
//     });
//     // Remove obstacles that are off-screen
//     if (obstacles.length && obstacles[0].x + obstacles[0].width < 0) {
//         obstacles.shift();
//         score++; // Increment score when an obstacle is passed
//     }
// }
// 
// function checkCollision() {
//     obstacles.forEach(obstacle => {
//         if (bird.x < obstacle.x + obstacle.width &&
//             bird.x + bird.width > obstacle.x &&
//             bird.y < obstacle.y + obstacle.height &&
//             bird.y + bird.height > obstacle.y) {
//             // Collision detected
//             score = 0; // Reset score
//             obstacles.length = 0; // Clear obstacles
//             clearInterval(obstacleInterval); // Clear the existing interval
//             obstacleInterval = null; // Reset the interval ID
//             startObstacleCreation(); // Start a new interval for obstacle creation
//             gameLoop(); // Restart the game loop
//         }
//     });
// }
// 
// const canvas = document.getElementById("gameCanvas");
// const ctx = canvas.getContext("2d");
// 
// let bird = { x: 50, y: 150, width: 20, height: 20, gravity: 0.6, lift: -15, velocity: 0 };
// 
// function drawBird() {
//     ctx.fillStyle = "yellow";
//     ctx.fillRect(bird.x, bird.y, bird.width, bird.height);
// }
// 
// function update() {
//     bird.velocity += bird.gravity;
//     bird.y += bird.velocity;
// 
//     if (bird.y + bird.height >= canvas.height) {
//         bird.y = canvas.height - bird.height;
//         bird.velocity = 0;
//     }
// 
//     if (bird.y < 0) {
//         bird.y = 0;
//         bird.velocity = 0;
//     }
// }
// 
// function gameLoop() {
//     ctx.clearRect(0, 0, canvas.width, canvas.height);
//     drawBird();
//     update();
//     drawObstacles(); // Draw obstacles
//     updateObstacles(); // Update obstacles
//     checkCollision(); // Check for collisions
//     requestAnimationFrame(gameLoop);
// }
// 
// document.addEventListener("keydown", () => {
//     bird.velocity += bird.lift;
// });
// // Start the obstacle creation interval when the game starts
// startObstacleCreation();
// 
// gameLoop();
// ```
// "#;
// 
// 
// 
//         let result = UnifiedDiffFormat::parse_message(input, Arc::new(PrivacySettings::allow_all())).await.expect(
//             "Failed to parse diff message"
//         );
//         print!("Result: {:?}\n", serde_json::to_string_pretty(&result));
// 
//         let (_, changed_text) = apply_diff(
//             &"/home/svakhreev/tmp/flappy_bird/game.js".to_string(),
//             &result,
//         );
// 
//         assert_eq!(changed_text, input);
//     }
}
