use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::Mutex as AMutex;
use tokio::task::JoinHandle;
use tracing::info;
use tree_sitter::Point;

use crate::ast::ast_index::AstIndex;
use crate::ast::ast_index_service::AstIndexService;
use crate::ast::ast_search_engine::AstSearchEngine;
use crate::ast::structs::{AstCursorSearchResult, AstQuerySearchResult, FileReferencesResult};
use crate::global_context::CommandLine;


#[derive(Debug)]
pub struct AstModule {
    ast_index_service: Arc<AMutex<AstIndexService>>,
    ast_index: Arc<AMutex<AstIndex>>,
    ast_search_engine: Arc<AMutex<AstSearchEngine>>,
    cmdline: CommandLine,
}

#[derive(Debug, Serialize)]
pub struct VecDbCaps {
    functions: Vec<String>,
}


impl AstModule {
    pub async fn init(
        cmdline: CommandLine,
    ) -> Result<AstModule, String> {
        let ast_index = Arc::new(AMutex::new(AstIndex::init()));
        let ast_search_engine = Arc::new(AMutex::new(AstSearchEngine::init(ast_index.clone())));
        let ast_index_service = Arc::new(AMutex::new(AstIndexService::init(ast_index.clone())));
        Ok(AstModule {
            ast_index_service,
            ast_index,
            ast_search_engine,
            cmdline,
        })
    }

    pub async fn start_background_tasks(&self) -> Vec<JoinHandle<()>> {
        info!("ast module: start_background_tasks");
        return self.ast_index_service.lock().await.start_background_tasks().await;
    }

    pub async fn ast_indexer_enqueue_files(&self, file_paths: &Vec<PathBuf>, force: bool) {
        self.ast_index_service.lock().await.ast_indexer_enqueue_files(file_paths, force).await;
    }

    pub async fn remove_file(&self, file_path: &PathBuf) {
        // TODO: will not work if the same file is in the indexer queue
        let _ = self.ast_index.lock().await.remove(file_path).await;
    }

    pub async fn search_by_cursor(
        &self,
        file_path: &PathBuf,
        code: &str,
        cursor: Point,
        top_n: usize
    ) -> Result<AstCursorSearchResult, String> {
        let t0 = std::time::Instant::now();

        let mut handler_locked = self.ast_search_engine.lock().await;
        let (results, cursor_symbols) = match handler_locked.search(
            file_path,
            code,
            cursor,
            top_n
        ).await {
            Ok(res) => res,
            Err(_) => { return Err("error during search occurred".to_string()); }
        };
        for rec in results.iter() {
            info!("distance {:.3}, found {}, ", rec.sim_to_query, rec.symbol_declaration.meta_path);
        }
        info!("search_by_cursor time {:.3}s, found {} results", t0.elapsed().as_secs_f32(), results.len());
        Ok(
            AstCursorSearchResult {
                query_text: code.to_string(),
                file_path: file_path.clone(),
                cursor: cursor,
                cursor_symbols: cursor_symbols,
                search_results: results,
            }
        )
    }

    pub async fn search_by_symbol_path(
        &self,
        symbol_path: String,
        top_n: usize
    ) -> Result<AstQuerySearchResult, String> {
        let t0 = std::time::Instant::now();
        let ast_index = self.ast_index.clone();
        let ast_index_locked  = ast_index.lock().await;
        match ast_index_locked.search(symbol_path.as_str(), top_n, None).await {
            Ok(results) => {
                for r in results.iter() {
                    info!("distance {:.3}, found {}, ", r.sim_to_query, r.symbol_declaration.meta_path);
                }
                info!("search_by_symbol_path time {:.3}s, found {} results", t0.elapsed().as_secs_f32(), results.len());
                Ok(
                    AstQuerySearchResult {
                        query_text: symbol_path,
                        search_results: results,
                    }
                )
            },
            Err(e) => Err(e.to_string())
        }
    }

    pub async fn get_file_references(&self, file_path: PathBuf) -> Result<FileReferencesResult, String> {
        let ast_index = self.ast_index.clone();
        let ast_index_locked  = ast_index.lock().await;
        let symbols = match ast_index_locked.get_symbols_by_file_path(&file_path) {
            Ok(s) => s,
            Err(err) => { return Err(format!("Error: {}", err)) }
        };
        Ok(FileReferencesResult {
            file_path: file_path.clone(),
            symbols,
        })
    }

    pub async fn get_indexed_symbol_paths(&self) -> Vec<String> {
        let ast_index = self.ast_index.clone();
        let ast_index_locked  = ast_index.lock().await;
        ast_index_locked.get_indexed_symbol_paths()
    }
}
