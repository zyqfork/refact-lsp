use std::sync::{Arc, Weak};
use tokio::sync::{Mutex as AMutex, Mutex, RwLock};
use md5;
use tokenizers::Tokenizer;
use tracing::info;
use crate::ast::treesitter::parsers::get_ast_parser_by_filename;
use std::sync::RwLock as StdRwLock;
use crate::call_validation::{ChatMessage, ContextFile};
use crate::files_in_workspace::Document;
use crate::global_context::GlobalContext;
use crate::scratchpads::chat_utils_rag::postprocess_at_results2;
use crate::vecdb::file_splitter::FileSplitter;
use crate::vecdb::structs::SplitResult;

fn str_hash(s: &String) -> String {
    let digest = md5::compute(s);
    format!("{:x}", digest)
}

pub struct AstBasedFileSplitter {
    soft_window: usize,
    // hard_window: usize,
    fallback_file_splitter: FileSplitter,
}

impl AstBasedFileSplitter {
    pub fn new(window_size: usize, soft_limit: usize) -> Self {
        Self {
            soft_window: window_size,
            // hard_window: window_size + soft_limit,
            fallback_file_splitter: FileSplitter::new(window_size, soft_limit),
        }
    }

    pub async fn split(&self, doc: &Document, 
                       tokenizer: Arc<StdRwLock<Tokenizer>>, 
                       global_context: Weak<RwLock<GlobalContext>>,
                       tokens_limit: usize
    ) -> Result<Vec<SplitResult>, String> {
        let mut doc = doc.clone();
        let path = doc.path.clone();
        let mut parser = match get_ast_parser_by_filename(&path) {
            Ok(parser) => parser,
            Err(_) => {
                info!("cannot find a parser for {:?}, using simple file splitter", path);
                return self.fallback_file_splitter.split(&doc).await;
            }
        };
        let text = match doc.get_text_or_read_from_disk().await {
            Ok(s) => s,
            Err(err) => {
                return Err(err.to_string());
            }
        };
        let symbols = parser.parse(text.as_str(), &path);
        let mut chunks = Vec::new();
        let mut split_normally: usize = 0;
        let mut split_using_fallback: usize = 0;
        let mut split_errors: usize = 0;
        for symbol in symbols.iter().map(|s| s.read()
            .expect("cannot read symbol")
            .symbol_info_struct()) {
            let mut content = match symbol.get_content().await {
                Ok(content) => content,
                Err(err) => {
                    split_errors += 1;
                    info!("cannot retrieve symbol's content {}", err);
                    continue;
                }
            };
            if let Some(gcx) = global_context.upgrade() {
                let full_range = symbol.full_range;
                let messages = vec![ChatMessage {
                    role: "user".to_string(),
                    content: serde_json::to_string(&vec![ContextFile {
                        file_name: symbol.file_path.to_str().unwrap().parse().unwrap(),
                        file_content: doc.text.clone().unwrap().to_string(),
                        line1: full_range.start_point.row,
                        line2: full_range.end_point.row,
                        symbol: "".to_string(),
                        gradient_type: 0,
                        usefulness: 0.0,
                    }]).unwrap(),
                }];
                let res = postprocess_at_results2(gcx.clone(), messages, tokenizer.clone(), tokens_limit).await;
                if let Some(first) = res.first() {
                    content = first.file_content.clone();
                }
            }
            
            if content.len() > self.soft_window {
                let mut temp_doc = Document::new(&doc.path, Some("unknown".to_string()));
                temp_doc.update_text(&content);
                match self.fallback_file_splitter.split(&temp_doc).await {
                    Ok(mut res) => {
                        for r in res.iter_mut() {
                            r.start_line += symbol.full_range.start_point.row as u64;
                            r.end_line += symbol.full_range.start_point.row as u64;
                        }
                        chunks.extend(res)
                    }
                    Err(err) => {
                        info!("{}", err);
                    }
                }
                split_using_fallback += 1;
                continue;
            } else {
                split_normally += 1;
                chunks.push(SplitResult {
                    file_path: doc.path.clone(),
                    window_text: content.clone(),
                    window_text_hash: str_hash(&content),
                    start_line: symbol.full_range.start_point.row as u64,
                    end_line: symbol.full_range.end_point.row as u64,
                });
            }
        }
        let last_30_chars = crate::nicer_logs::last_n_chars(&doc.path.display().to_string(), 30);
        let message = format!("split {last_30_chars} by definitions {split_normally}, fallback {split_using_fallback}, errors {split_errors}");
        info!(message);

        Ok(chunks)
    }
}
