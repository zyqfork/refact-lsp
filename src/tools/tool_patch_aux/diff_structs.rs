use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use itertools::Itertools;
use crate::call_validation::DiffChunk;

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
                    .map(|x| x.file_line_num_idx
                        .clone()
                        .expect("All file_line_num_idx must be filled to this moment in the `normalize_diff_block` func") + 1)
                    .min()
                    .unwrap_or(1),
                line2: useful_block_lines
                    .iter()
                    .map(|x| {
                        if x.line_type == LineType::Plus {
                            x.file_line_num_idx.clone()
                                .expect("All file_line_num_idx must be filled to this moment in the `normalize_diff_block` func") + 1
                        } else {
                            x.file_line_num_idx
                                .clone()
                                .expect("All file_line_num_idx must be filled to this moment in the `normalize_diff_block` func") + 2
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