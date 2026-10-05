use super::*;
use jieba_rs::Jieba;
use rusqlite::{params, Connection, OptionalExtension};
use std::sync::{Once, OnceLock};
const CHUNK_FORMAT: &str = "guide-structure-v2-budgeted-context";
type SqliteExtensionEntry = unsafe extern "C" fn(
    *mut rusqlite::ffi::sqlite3,
    *mut *const std::os::raw::c_char,
    *const rusqlite::ffi::sqlite3_api_routines,
) -> std::os::raw::c_int;
type Row = (
    String,
    String,
    String,
    String,
    u64,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    usize,
);
static VEC_INIT: Once = Once::new();
static DEFAULT_JIEBA: OnceLock<Arc<Jieba>> = OnceLock::new();
static CUSTOM_JIEBA: OnceLock<Mutex<BTreeMap<String, Arc<Jieba>>>> = OnceLock::new();
struct BuildGuard<'a>(&'a AtomicBool);
impl Drop for BuildGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chunk {
    pub id: String,
    pub section: String,
    pub ordinal: usize,
    pub text: String,
    pub embedding_text: String,
    pub content_hash: String,
    pub previous: Option<String>,
    pub next: Option<String>,
}
pub fn chunks(m: &Memory, max_bytes: usize) -> Vec<Chunk> {
    // The retrieval layout uses at least 64 bytes; smaller configured model
    // budgets explicitly disable semantic indexing while keyword search works.
    let max_bytes = max_bytes.max(64);
    let mut blocks: Vec<(String, String)> = Vec::new();
    let mut section = String::new();
    let mut block = String::new();
    let mut fenced = false;
    for line in m.body.lines() {
        if line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~") {
            fenced = !fenced;
        }
        let heading =
            !fenced && line.starts_with('#') && line.trim_start_matches('#').starts_with(' ');
        if heading {
            if !block.trim().is_empty() {
                blocks.push((section.clone(), std::mem::take(&mut block)));
            }
            section = line
                .trim_start_matches('#')
                .trim()
                .chars()
                .take(256)
                .collect();
        } else if line.trim().is_empty() && !fenced {
            if !block.trim().is_empty() {
                blocks.push((section.clone(), std::mem::take(&mut block)));
            }
            continue;
        }
        block.push_str(line);
        block.push('\n');
    }
    if !block.trim().is_empty() {
        blocks.push((section, block));
    }
    let mut pieces: Vec<(String, String)> = Vec::new();
    for (section, block) in blocks {
        let available = max_bytes - embedding_context(m, &section, max_bytes).len();
        for piece in split_block(&block, available) {
            if let Some(last) = pieces.last_mut() {
                if last.0 == section && last.1.len() + piece.len() + 2 <= available {
                    last.1.push('\n');
                    last.1.push_str(&piece);
                    continue;
                }
            }
            pieces.push((section.clone(), piece));
        }
    }
    let mut out: Vec<_> = pieces
        .into_iter()
        .enumerate()
        .map(|(i, (section, text))| {
            let embedding_text = format!("{}{text}", embedding_context(m, &section, max_bytes));
            let content_hash = hash(&embedding_text);
            Chunk {
                id: format!("{}-{i}", m.id),
                section,
                ordinal: i,
                text,
                embedding_text,
                content_hash,
                previous: None,
                next: None,
            }
        })
        .collect();
    let ids: Vec<_> = out.iter().map(|c| c.id.clone()).collect();
    for (i, c) in out.iter_mut().enumerate() {
        c.previous = i.checked_sub(1).map(|j| ids[j].clone());
        c.next = ids.get(i + 1).cloned();
    }
    out
}
fn utf8_prefix(text: &str, max: usize) -> &str {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
pub(super) fn source_summaries(sources: &[Value]) -> (Vec<Value>, bool) {
    const MAX_SOURCES: usize = 8;
    const MAX_BYTES: usize = 4096;
    let mut summaries = Vec::new();
    let mut bytes = 2usize; // JSON array delimiters.
    let mut truncated = sources.len() > MAX_SOURCES;
    for (position, source) in sources.iter().take(MAX_SOURCES).enumerate() {
        let mut summary = serde_json::Map::new();
        let mut shortened = false;
        if let Some(fields) = source.as_object() {
            for (key, value) in fields {
                let limit = match key.as_str() {
                    "id" | "source_id" | "conversation_id" | "message_id" | "session_id"
                    | "source_session_id" | "run_id" | "chunk_id" => 256,
                    "section" | "filename" => 512,
                    "url" | "source_url" => 1024,
                    "type" => 128,
                    "title" | "excerpt" => 384,
                    "revision" | "source_revision" | "line" | "page" => {
                        if value.as_u64().is_some() {
                            summary.insert(key.clone(), value.clone());
                        } else {
                            shortened = true;
                        }
                        continue;
                    }
                    _ => {
                        shortened = true;
                        continue;
                    }
                };
                if let Some(text) = value.as_str() {
                    if text.len() <= limit {
                        summary.insert(key.clone(), value.clone());
                    } else {
                        shortened = true;
                        // Identity fields must never become fabricated IDs or
                        // URLs. Full provenance remains available via memory_get.
                        if matches!(key.as_str(), "title" | "excerpt") {
                            summary.insert(key.clone(), json!(utf8_prefix(text, limit)));
                        }
                    }
                } else {
                    shortened = true;
                }
            }
        } else {
            shortened = true;
        }
        summary.insert("source_index".into(), json!(position));
        if shortened {
            summary.insert("truncated".into(), json!(true));
        }
        let summary = Value::Object(summary);
        let size = summary.to_string().len() + usize::from(!summaries.is_empty());
        if bytes + size > MAX_BYTES {
            truncated = true;
            break;
        }
        bytes += size;
        truncated |= shortened;
        summaries.push(summary);
    }
    (summaries, truncated)
}
fn embedding_context(m: &Memory, section: &str, max: usize) -> String {
    let full = format!(
        "标题：{}\n版本：{}\n适用条件：{}\n章节：{}\n\n",
        m.title, m.game_version, m.applicability, section
    );
    let budget = max / 2;
    if full.len() <= budget {
        return full;
    }
    let labels = ["标题：", "版本：", "适用条件：", "章节："];
    let values = [
        m.title.as_str(),
        m.game_version.as_str(),
        m.applicability.as_str(),
        section,
    ];
    let minimum = labels.iter().map(|s| s.len() + 1).sum::<usize>() + "[省略]\n".len() + 1;
    if minimum > budget {
        return "[上下文省略]\n".to_string();
    }
    let mut remaining = budget - minimum;
    let mut out = String::from("[省略]\n");
    for (i, (label, value)) in labels.iter().zip(values).enumerate() {
        let selected = utf8_prefix(value, remaining / (labels.len() - i));
        remaining -= selected.len();
        out.push_str(label);
        out.push_str(selected);
        out.push('\n');
    }
    out.push('\n');
    out
}
fn split_block(block: &str, max: usize) -> Vec<String> {
    if block.len() <= max {
        return vec![block.trim().to_string()];
    }
    // Tables repeat their first two rows (header + delimiter), grouping complete
    // rows whenever possible. Overlong individual rows have an explicit split.
    let lines: Vec<_> = block.lines().collect();
    let table = lines.len() > 2
        && lines[0].contains('|')
        && lines[1].contains('|')
        && lines[1]
            .chars()
            .all(|c| c.is_whitespace() || "|:-".contains(c));
    let repeated_header = if table {
        format!("{}\n{}\n", lines[0], lines[1])
    } else {
        String::new()
    };
    // An oversized header cannot fit with a UTF-8 row. Keep all its lines as
    // ordinary fragments rather than discarding them or exceeding the budget.
    let table = table && repeated_header.len() + 4 <= max;
    let header = if table {
        repeated_header
    } else {
        String::new()
    };
    let mut out = Vec::new();
    let mut current = header.clone();
    for line in lines.iter().skip(if table { 2 } else { 0 }) {
        if current.len() + line.len() + 1 > max && !current.trim().is_empty() && current != header {
            out.push(current.trim_end().into());
            current = header.clone();
        }
        if line.len() + header.len() + 1 > max {
            if current != header && !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            for part in split_utf8(line, max.saturating_sub(header.len()).max(1)) {
                out.push(format!("{header}{part}"));
            }
            current = header.clone();
        } else {
            current.push_str(line);
            current.push('\n');
        }
    }
    if current != header && !current.trim().is_empty() {
        out.push(current.trim_end().into());
    }
    out
}
fn split_utf8(text: &str, max: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut part = String::new();
    for c in text.chars() {
        if part.len() + c.len_utf8() > max && !part.is_empty() {
            parts.push(std::mem::take(&mut part));
        }
        part.push(c);
    }
    if !part.is_empty() {
        parts.push(part)
    }
    parts
}
fn vec_blob(v: &[f32]) -> Result<Vec<u8>> {
    ensure!(
        !v.is_empty()
            && v.len() <= 65536
            && v.iter().all(|f| f.is_finite())
            && v.iter().any(|f| *f != 0.0),
        "memory.embedding_vector_invalid"
    );
    Ok(v.iter().flat_map(|f| f.to_le_bytes()).collect())
}
struct Segmenter {
    jieba: Arc<Jieba>,
    dictionary: Dictionary,
}
impl Segmenter {
    fn new(dictionary: Dictionary) -> Self {
        if dictionary.terms.is_empty() && dictionary.aliases.is_empty() {
            return Self {
                jieba: DEFAULT_JIEBA.get_or_init(|| Arc::new(Jieba::new())).clone(),
                dictionary,
            };
        }
        let identity = hash(&serde_json::to_string(&dictionary).expect("dictionary serializes"));
        let mut cached = CUSTOM_JIEBA
            .get_or_init(|| Mutex::new(BTreeMap::new()))
            .lock();
        if let Some(jieba) = cached.get(&identity) {
            return Self {
                jieba: jieba.clone(),
                dictionary,
            };
        }
        let mut jieba = Jieba::new();
        for term in dictionary
            .terms
            .iter()
            .chain(dictionary.aliases.keys())
            .chain(dictionary.aliases.values().flatten())
        {
            jieba.add_word(term, Some(10000), None);
        }
        let jieba = Arc::new(jieba);
        if cached.len() >= 2 {
            if let Some(old) = cached.keys().next().cloned() {
                cached.remove(&old);
            }
        }
        cached.insert(identity, jieba.clone());
        Self { jieba, dictionary }
    }
    fn words(&self, text: &str) -> String {
        let mut words: BTreeSet<String> = self
            .jieba
            .cut_for_search(text, true)
            .iter()
            .filter_map(|w| {
                let w = w.trim().to_lowercase();
                (!w.is_empty() && w.chars().all(|c| c.is_alphanumeric() || c == '_')).then_some(w)
            })
            .collect();
        for (canonical, aliases) in &self.dictionary.aliases {
            if text.contains(canonical) || aliases.iter().any(|a| text.contains(a)) {
                words.insert(canonical.to_lowercase());
                words.extend(aliases.iter().map(|a| a.to_lowercase()));
            }
        }
        words.into_iter().collect::<Vec<_>>().join(" ")
    }
    fn aliases(&self, m: &Memory) -> String {
        let context = format!("{} {} {}", m.title, m.tags.join(" "), m.body);
        self.dictionary
            .aliases
            .iter()
            .filter(|(c, a)| context.contains(c.as_str()) || a.iter().any(|v| context.contains(v)))
            .flat_map(|(c, a)| std::iter::once(c.clone()).chain(a.iter().cloned()))
            .collect::<Vec<_>>()
            .join(" ")
    }
    fn query(&self, text: &str) -> String {
        let words = self.words(text);
        let bigrams = bigrams(text);
        let mut terms: BTreeSet<String> = words.split_whitespace().map(|s| s.to_owned()).collect();
        terms.extend(bigrams.split_whitespace().map(str::to_owned));
        terms
            .into_iter()
            .take(64)
            .map(|s| format!("\"{}\"", s.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ")
    }
}
fn bigrams(text: &str) -> String {
    let mut words = BTreeSet::new();
    let mut previous = None;
    for c in text.chars() {
        if ('\u{3400}'..='\u{9fff}').contains(&c) {
            if let Some(p) = previous {
                words.insert(format!("{p}{c}"));
            }
            previous = Some(c);
        } else {
            previous = None
        }
    }
    words.into_iter().collect::<Vec<_>>().join(" ")
}
impl MemoryStore {
    fn index_path(&self, pkg: &str) -> PathBuf {
        self.root
            .join("cache/memory-index")
            .join(format!("{pkg}.sqlite"))
    }
    pub(super) fn open_index(&self, pkg: &str) -> Result<Connection> {
        VEC_INIT.call_once(|| unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                SqliteExtensionEntry,
            >(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        });
        let p = self.index_path(pkg);
        std::fs::create_dir_all(p.parent().unwrap())?;
        let open = || -> Result<Connection> {
            let db = Connection::open(&p)?;
            db.busy_timeout(std::time::Duration::from_secs(3))?;
            let check: String = db.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
            ensure!(check == "ok", "memory.index_corrupt");
            Ok(db)
        };
        let db = match open() {
            Ok(db) => db,
            Err(error) => {
                if p.exists() {
                    let quarantine = self.root.join("cache/memory-index-corrupt").join(pkg);
                    std::fs::create_dir_all(&quarantine)?;
                    std::fs::rename(
                        &p,
                        quarantine.join(format!("{}.sqlite", uuid::Uuid::new_v4())),
                    )?;
                }
                tracing::warn!(%error,"AI memory cache rebuilt after corruption");
                open()?
            }
        };
        db.query_row("SELECT vec_version()", [], |r| r.get::<_, String>(0))
            .context("memory.sqlite_vec_unavailable")?;
        db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA foreign_keys=ON; PRAGMA secure_delete=ON;
          CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS memories(id TEXT PRIMARY KEY,version TEXT NOT NULL,revision INTEGER NOT NULL,title TEXT NOT NULL,summary TEXT NOT NULL,kind TEXT NOT NULL,tags TEXT NOT NULL,applicability TEXT NOT NULL,game_version TEXT NOT NULL,validation TEXT NOT NULL,status TEXT NOT NULL,sources TEXT NOT NULL,protected INTEGER NOT NULL DEFAULT 0);
          CREATE TABLE IF NOT EXISTS chunks(id TEXT PRIMARY KEY,memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,section TEXT NOT NULL,ordinal INTEGER NOT NULL,text TEXT NOT NULL,embedding_text TEXT NOT NULL,content_hash TEXT NOT NULL,embedding BLOB,embedding_fingerprint TEXT,embedding_dim INTEGER);
          CREATE INDEX IF NOT EXISTS chunk_memory ON chunks(memory_id);
          CREATE VIRTUAL TABLE IF NOT EXISTS chunk_fts USING fts5(chunk_id UNINDEXED,title_words,alias_words,keyword_words,body_words,bigrams,tokenize='unicode61');")?;
        let has_protected: bool = db.query_row(
            "SELECT count(*)>0 FROM pragma_table_info('memories') WHERE name='protected'",
            [],
            |r| r.get(0),
        )?;
        if !has_protected {
            db.execute_batch("ALTER TABLE memories ADD COLUMN protected INTEGER NOT NULL DEFAULT 0; DELETE FROM meta WHERE key='dictionary';")?;
        }
        let has_source_review: bool = db.query_row(
            "SELECT count(*)>0 FROM pragma_table_info('memories') WHERE name='source_review'",
            [],
            |r| r.get(0),
        )?;
        if !has_source_review {
            db.execute_batch("ALTER TABLE memories ADD COLUMN source_review INTEGER NOT NULL DEFAULT 0; DELETE FROM meta WHERE key='dictionary';")?;
        }
        Ok(db)
    }
    pub(super) fn sync_index(&self, pkg: &str) -> Result<Value> {
        self.sync_index_chunks(pkg, None)
    }
    fn sync_index_chunks(&self, pkg: &str, requested: Option<usize>) -> Result<Value> {
        let _guard = self.gate.lock();
        self.packages.with_package_read(pkg,||{
            self.reconcile_permanent_deletions(pkg)?;
            let (memories,diagnostics)=self.snapshot(pkg)?;let (dictionary,_)=self.dictionary(pkg)?;
            let segmenter=Segmenter::new(dictionary.clone());let dictionary_hash=hash(&serde_json::to_string(&dictionary)?);
            let mut db=self.open_index(pkg)?;let tx=db.transaction()?;
            let prior_chunk_bytes:Option<String>=tx.query_row("SELECT value FROM meta WHERE key='chunk_bytes'",[],|r|r.get(0)).optional()?;
            let chunk_bytes=requested.or_else(||prior_chunk_bytes.as_ref().and_then(|s|s.parse().ok())).unwrap_or(24*1024).clamp(64,24*1024);
            let chunk_layout=format!("{CHUNK_FORMAT}:{chunk_bytes}");
            let prior_chunk_layout:Option<String>=tx.query_row("SELECT value FROM meta WHERE key='chunk_layout'",[],|r|r.get(0)).optional()?;
            let prior_dictionary=tx.query_row("SELECT value FROM meta WHERE key='dictionary'",[],|r|r.get::<_,String>(0)).optional()?;
            let mut ids=BTreeSet::new();
            for (m,version) in &memories {
                ids.insert(m.id.clone());
                let source_review=self.source_review_required(pkg,m)?;
                let old=tx.query_row("SELECT version,source_review FROM memories WHERE id=?1",[&m.id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,bool>(1)?))).optional()?;
                if old.as_ref().is_some_and(|(v,s)|v==version&&*s==source_review)&&prior_dictionary.as_deref()==Some(&dictionary_hash)&&prior_chunk_layout.as_deref()==Some(&chunk_layout){continue}
                tx.execute("INSERT INTO memories(id,version,revision,title,summary,kind,tags,applicability,game_version,validation,status,sources,protected) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13) ON CONFLICT(id) DO UPDATE SET version=excluded.version,revision=excluded.revision,title=excluded.title,summary=excluded.summary,kind=excluded.kind,tags=excluded.tags,applicability=excluded.applicability,game_version=excluded.game_version,validation=excluded.validation,status=excluded.status,sources=excluded.sources,protected=excluded.protected",
                    params![m.id,version,m.revision,m.title,m.body.chars().take(220).collect::<String>(),m.kind,serde_json::to_string(&m.tags)?,m.applicability,m.game_version,m.validation,m.status,serde_json::to_string(&m.sources)?,!m.protected_fields.is_empty()])?;
                tx.execute("UPDATE memories SET source_review=?1 WHERE id=?2",params![source_review,m.id])?;
                let chunks=chunks(m,chunk_bytes);let valid:BTreeSet<_>=chunks.iter().map(|c|c.id.clone()).collect();
                let old_chunks:Vec<String>=tx.prepare("SELECT id FROM chunks WHERE memory_id=?1")?.query_map([&m.id],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
                for c in old_chunks {if !valid.contains(&c){tx.execute("DELETE FROM chunk_fts WHERE chunk_id=?1",[&c])?;tx.execute("DELETE FROM chunks WHERE id=?1",[&c])?;}}
                let alias_words=segmenter.words(&segmenter.aliases(m));
                for c in chunks {
                    tx.execute("INSERT INTO chunks(id,memory_id,section,ordinal,text,embedding_text,content_hash) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(id) DO UPDATE SET section=excluded.section,ordinal=excluded.ordinal,text=excluded.text,embedding_text=excluded.embedding_text,embedding=CASE WHEN chunks.content_hash=excluded.content_hash THEN chunks.embedding ELSE NULL END,embedding_fingerprint=CASE WHEN chunks.content_hash=excluded.content_hash THEN chunks.embedding_fingerprint ELSE NULL END,embedding_dim=CASE WHEN chunks.content_hash=excluded.content_hash THEN chunks.embedding_dim ELSE NULL END,content_hash=excluded.content_hash",
                        params![c.id,m.id,c.section,c.ordinal,c.text,c.embedding_text,c.content_hash])?;
                    tx.execute("DELETE FROM chunk_fts WHERE chunk_id=?1",[&c.id])?;
                    tx.execute("INSERT INTO chunk_fts(chunk_id,title_words,alias_words,keyword_words,body_words,bigrams) VALUES(?1,?2,?3,?4,?5,?6)",
                        params![c.id,segmenter.words(&m.title),alias_words,segmenter.words(&format!("{} {}",m.tags.join(" "),m.applicability)),segmenter.words(&c.text),bigrams(&format!("{} {}",m.title,c.text))])?;
                }
            }
            let old_ids:Vec<String>=tx.prepare("SELECT id FROM memories")?.query_map([],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
            for old in old_ids {if !ids.contains(&old){tx.execute("DELETE FROM chunk_fts WHERE chunk_id IN (SELECT id FROM chunks WHERE memory_id=?1)",[&old])?;tx.execute("DELETE FROM memories WHERE id=?1",[old])?;}}
            tx.execute("INSERT OR REPLACE INTO meta(key,value) VALUES('dictionary',?1)",[dictionary_hash])?;
            tx.execute("INSERT OR REPLACE INTO meta(key,value) VALUES('chunk_bytes',?1)",[chunk_bytes.to_string()])?;
            tx.execute("INSERT OR REPLACE INTO meta(key,value) VALUES('chunk_layout',?1)",[chunk_layout])?;
            tx.execute("INSERT OR REPLACE INTO meta(key,value) VALUES('diagnostics',?1)",[serde_json::to_string(&diagnostics)?])?;
            tx.commit()?;self.status_db(&db)
        })
    }
    fn status_db(&self, db: &Connection) -> Result<Value> {
        let fingerprint: Option<String> = db
            .query_row(
                "SELECT value FROM meta WHERE key='embedding_fingerprint'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let total_memories: u64 =
            db.query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))?;
        let total_chunks: u64 = db.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;
        let pending:u64=db.query_row("SELECT COUNT(*) FROM chunks c JOIN memories m ON m.id=c.memory_id WHERE m.status='active' AND m.validation!='invalid' AND (c.embedding IS NULL OR c.embedding_fingerprint!=?1 OR ?1 IS NULL)",params![fingerprint],|r|r.get(0))?;
        let diagnostics: Value = db
            .query_row("SELECT value FROM meta WHERE key='diagnostics'", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(json!([]));
        let chunk_bytes: Option<String> = db
            .query_row("SELECT value FROM meta WHERE key='chunk_bytes'", [], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(
            json!({"keyword_ready":true,"semantic_ready":fingerprint.is_some()&&pending==0,"pending_vectors":pending,"total_memories":total_memories,"total_chunks":total_chunks,"chunk_bytes":chunk_bytes.and_then(|s|s.parse::<usize>().ok()),"embedding_fingerprint":fingerprint,"diagnostics":diagnostics}),
        )
    }
    pub(super) fn index_status(&self, pkg: &str) -> Result<Value> {
        self.sync_index(pkg)
    }
    pub(super) fn index_status_connection(
        &self,
        pkg: &str,
        connection: Option<&ServiceConnection>,
    ) -> Result<Value> {
        let mut status = self.sync_index_chunks(
            pkg,
            connection
                .filter(|c| c.embedding_enabled())
                .map(ServiceConnection::embedding_chunk_bytes),
        )?;
        if let Some(connection) = connection.filter(|c| c.embedding_enabled()) {
            if connection.embedding_chunk_bytes() < 64 {
                status["semantic_ready"] = json!(false);
                status["degraded_reason"] = json!({"code":"embedding_context_budget_too_small","available_bytes":connection.embedding_chunk_bytes(),"detail":"编码前缀后不足64字节，关键词检索可用；请缩短前缀或提高输入预算"});
                return Ok(status);
            }
            let _guard = self.gate.lock();
            let db = self.open_index(pkg)?;
            db.execute(
                "INSERT OR REPLACE INTO meta(key,value) VALUES('embedding_fingerprint',?1)",
                [Self::fingerprint(connection)],
            )?;
            status = self.status_db(&db)?;
        } else {
            status["semantic_ready"] = json!(false);
            status["degraded_reason"] = json!({"code":"embedding_not_configured","detail":"语义检索未配置，关键词检索可用"});
        }
        Ok(status)
    }
    fn fingerprint(connection: &ServiceConnection) -> String {
        hash(&format!(
            "{}:{CHUNK_FORMAT}",
            connection.embedding_fingerprint()
        ))
    }
    pub(super) async fn rebuild(
        &self,
        pkg: &str,
        embedding: Option<&ServiceConnection>,
        cancel: &AtomicBool,
    ) -> Result<Value> {
        if self
            .index_building
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            let mut status = self.index_status_connection(pkg, embedding)?;
            status["indexing"] = json!(true);
            return Ok(status);
        }
        let _building = BuildGuard(&self.index_building);
        self.sync_index_chunks(
            pkg,
            embedding
                .filter(|c| c.embedding_enabled())
                .map(ServiceConnection::embedding_chunk_bytes),
        )?;
        let mut usages = Vec::new();
        let mut error = None;
        if let Some(connection) = embedding.filter(|e| e.embedding_enabled()) {
            if connection.embedding_chunk_bytes() < 64 {
                let mut status = self.sync_index(pkg)?;
                status["semantic_ready"] = json!(false);
                status["degraded_reason"] = json!({"code":"embedding_context_budget_too_small","available_bytes":connection.embedding_chunk_bytes(),"detail":"编码前缀后不足64字节，关键词检索可用；请缩短前缀或提高输入预算"});
                status["embedding_usage"] = json!([]);
                return Ok(status);
            }
            let fingerprint = Self::fingerprint(connection);
            {
                let _guard = self.gate.lock();
                let db = self.open_index(pkg)?;
                db.execute(
                    "INSERT OR REPLACE INTO meta(key,value) VALUES('embedding_fingerprint',?1)",
                    [&fingerprint],
                )?;
            }
            {
                ensure!(!cancel.load(Ordering::Acquire), "memory.cancelled");
                let pending = {
                    let _guard = self.gate.lock();
                    let db = self.open_index(pkg)?;
                    let mut stmt=db.prepare("SELECT c.id,c.content_hash,c.embedding_text FROM chunks c JOIN memories m ON m.id=c.memory_id WHERE m.status='active' AND m.validation!='invalid' AND (c.embedding IS NULL OR c.embedding_fingerprint!=?1) ORDER BY c.id LIMIT ?2")?;
                    let rows = stmt.query_map(
                        params![fingerprint, connection.embedding_max_batch_size() as i64],
                        |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, String>(2)?,
                            ))
                        },
                    )?;
                    rows.collect::<rusqlite::Result<Vec<_>>>()?
                };
                if !pending.is_empty() {
                    let texts: Vec<_> = pending.iter().map(|p| p.2.clone()).collect();
                    match connection
                        .embed(&texts, EmbeddingPurpose::Document, cancel)
                        .await
                    {
                        Ok(result) => {
                            usages.push(json!({"purpose":"document","usage":result.usage,"diagnostics":result.diagnostics}));
                            ensure!(
                                result.vectors.len() == pending.len(),
                                "memory.embedding_result_count"
                            );
                            // Reconcile all current JSON versions before accepting any network result.
                            self.sync_index(pkg)?;
                            let _guard = self.gate.lock();
                            self.packages.with_package_read(pkg,||{
                            let mut db=self.open_index(pkg)?;let tx=db.transaction()?;
                            let active:Option<String>=tx.query_row("SELECT value FROM meta WHERE key='embedding_fingerprint'",[],|r|r.get(0)).optional()?;
                            if active.as_deref()!=Some(&fingerprint){return Ok(())}
                            let dimension:Option<String>=tx.query_row("SELECT value FROM meta WHERE key='embedding_dimension'",[],|r|r.get(0)).optional()?;
                            if dimension.as_deref()!=Some(&result.dimensions.to_string()){tx.execute("UPDATE chunks SET embedding=NULL,embedding_fingerprint=NULL,embedding_dim=NULL",[])?;}
                            tx.execute("INSERT OR REPLACE INTO meta(key,value) VALUES('embedding_dimension',?1)",[result.dimensions.to_string()])?;
                            for ((chunk_id,content_hash,_),vector) in pending.iter().zip(&result.vectors) {
                                ensure!(vector.len()==result.dimensions,"memory.embedding_dimension_mismatch");
                                tx.execute("UPDATE chunks SET embedding=?1,embedding_fingerprint=?2,embedding_dim=?3 WHERE id=?4 AND content_hash=?5 AND EXISTS(SELECT 1 FROM memories m WHERE m.id=chunks.memory_id AND m.status='active' AND m.validation!='invalid')",params![vec_blob(vector)?,fingerprint,result.dimensions,chunk_id,content_hash])?;
                            }tx.commit()?;Ok(())
                        })?;
                            // One bounded batch per invocation. The host schedules the
                            // next batch after foreground requests have had a chance.
                        }
                        Err(failure) => {
                            error = Some(super::super::services::error_details(&failure));
                        }
                    }
                }
            }
        } else {
            error = Some(
                json!({"code":"embedding_not_configured","detail":"语义检索未配置，关键词检索可用"}),
            );
        }
        let mut status = self.sync_index(pkg)?;
        let out = status.as_object_mut().unwrap();
        out.insert("embedding_usage".into(), json!(usages));
        if let Some(error) = error {
            out.insert("degraded_reason".into(), error);
            out.insert("semantic_ready".into(), json!(false));
        }
        Ok(status)
    }
    pub(super) async fn search(
        &self,
        pkg: &str,
        args: &Value,
        embedding: Option<&ServiceConnection>,
        cancel: &AtomicBool,
    ) -> Result<Value> {
        let query = required(args, "query")?;
        ensure!(query.len() <= 24 * 1024, "memory.query_too_large");
        let mode = args.get("mode").and_then(Value::as_str).unwrap_or("hybrid");
        ensure!(
            ["hybrid", "keyword", "vector"].contains(&mode),
            "memory.search_mode_invalid"
        );
        let mut status = self.sync_index(pkg)?;
        let mut usages = Vec::new();
        let mut query_vector = None;
        let mut fingerprint = None;
        let mut degraded = None;
        if mode != "keyword" {
            if let Some(connection) = embedding.filter(|e| e.embedding_enabled()) {
                status = self.rebuild(pkg, Some(connection), cancel).await?;
                if let Some(v) = status.get("embedding_usage").and_then(Value::as_array) {
                    usages.extend(v.clone());
                }
                if let Some(v) = status.get("degraded_reason") {
                    degraded = Some(v.clone());
                }
                match connection
                    .embed(&[query.into()], EmbeddingPurpose::Query, cancel)
                    .await
                {
                    Ok(result) => {
                        usages.push(json!({"purpose":"query","usage":result.usage,"diagnostics":result.diagnostics}));
                        fingerprint = Some(Self::fingerprint(connection));
                        query_vector = result.vectors.into_iter().next();
                    }
                    Err(error) => degraded = Some(super::super::services::error_details(&error)),
                }
            } else {
                degraded = Some(
                    json!({"code":"embedding_not_configured","detail":"语义检索未配置，使用关键词检索"}),
                );
            }
        }
        self.sync_index(pkg)?;
        let _guard = self.gate.lock();
        self.packages.with_package_read(pkg,||{
            let db=self.open_index(pkg)?;let (dictionary,_)=self.dictionary(pkg)?;let segmenter=Segmenter::new(dictionary);
            let validation=args.get("validation").and_then(Value::as_str).unwrap_or("verified");ensure!(["any","pending","verified","invalid"].contains(&validation),"memory.validation_filter_invalid");
            let game_version=args.get("game_version").and_then(Value::as_str);let inactive=args.get("include_inactive").and_then(Value::as_bool).unwrap_or(false);
            let kind=args.get("kind").and_then(Value::as_str).unwrap_or("any");ensure!(["any","definition","pitfall","procedure"].contains(&kind),"memory.kind_filter_invalid");
            let protected=args.get("protected_only").and_then(Value::as_bool).unwrap_or(false);
            let candidates=args.get("candidates").and_then(Value::as_u64).unwrap_or(30).clamp(5,100) as usize;
            // Both branches apply the same eligibility BEFORE ORDER BY/LIMIT.
            let filter="(?1 OR m.status='active') AND (?2='any' OR (m.validation=?2 AND (?2!='verified' OR m.source_review=0))) AND (?3 IS NULL OR m.game_version=?3) AND (?4='any' OR m.kind=?4) AND (NOT ?5 OR m.protected=1)";
            let mut keyword:Vec<String>=Vec::new();let mut semantic:Vec<String>=Vec::new();
            if mode!="vector" {
                let fts=segmenter.query(query);
                if !fts.is_empty(){
                    let sql=format!("WITH ranked AS MATERIALIZED (SELECT c.id,c.memory_id,bm25(chunk_fts,0,12,10,7,3,0.3) AS score FROM chunk_fts JOIN chunks c ON c.id=chunk_fts.chunk_id JOIN memories m ON m.id=c.memory_id WHERE {filter} AND chunk_fts MATCH ?6) SELECT id FROM (SELECT id,score,ROW_NUMBER() OVER(PARTITION BY memory_id ORDER BY score,id) AS position FROM ranked) WHERE position<=3 ORDER BY score,id LIMIT ?7");
                    keyword=db.prepare(&sql)?.query_map(params![inactive,validation,game_version,kind,protected,fts,candidates],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
                }
                if query.chars().count()==1&&keyword.is_empty(){
                    let sql=format!("SELECT c.id FROM chunks c JOIN memories m ON m.id=c.memory_id WHERE {filter} AND (instr(m.title,?6)>0 OR instr(c.text,?6)>0) ORDER BY m.id,c.ordinal LIMIT ?7");
                    keyword=db.prepare(&sql)?.query_map(params![inactive,validation,game_version,kind,protected,query,candidates.min(20)],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
                }
            }
            let mut semantic_used=false;
            if let (Some(vector),Some(fp))=(&query_vector,&fingerprint) {
                let ready_sql=format!("SELECT COUNT(*) FROM chunks c JOIN memories m ON m.id=c.memory_id WHERE {filter} AND c.embedding_fingerprint=?6 AND c.embedding_dim=?7 AND c.embedding IS NOT NULL");
                let ready:u64=db.query_row(&ready_sql,params![inactive,validation,game_version,kind,protected,fp,vector.len()],|r|r.get(0))?;
                semantic_used=ready>0;
                let max_distance=args.get("max_distance").and_then(Value::as_f64).unwrap_or(0.85);ensure!((0.0..=2.0).contains(&max_distance),"memory.max_distance_invalid");
                let sql=format!("WITH ranked AS MATERIALIZED (SELECT c.id,c.memory_id,vec_distance_cosine(c.embedding,?8) AS score FROM chunks c JOIN memories m ON m.id=c.memory_id WHERE {filter} AND c.embedding_fingerprint=?6 AND c.embedding_dim=?7 AND c.embedding IS NOT NULL AND vec_distance_cosine(c.embedding,?8)<=?9) SELECT id FROM (SELECT id,score,ROW_NUMBER() OVER(PARTITION BY memory_id ORDER BY score,id) AS position FROM ranked) WHERE position<=3 ORDER BY score,id LIMIT ?10");
                semantic=db.prepare(&sql)?.query_map(params![inactive,validation,game_version,kind,protected,fp,vector.len(),vec_blob(vector)?,max_distance,candidates],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
            }
            let mut ranks:BTreeMap<String,f64>=BTreeMap::new();
            for branch in [&keyword,&semantic] {for (rank,chunk_id) in branch.iter().enumerate(){*ranks.entry(chunk_id.clone()).or_default()+=1.0/(60.0+rank as f64+1.0);}}
            let mut ranked:Vec<_>=ranks.into_iter().collect();ranked.sort_by(|a,b|b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            let mut seen=BTreeSet::new();let mut items=Vec::new();
            for (chunk_id,score) in ranked {
                let row:Row=db.query_row("SELECT m.id,m.title,m.summary,m.applicability,m.revision,m.version,m.game_version,m.validation,m.status,m.sources,c.section,c.text,c.ordinal FROM chunks c JOIN memories m ON m.id=c.memory_id WHERE c.id=?1",[&chunk_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?,r.get(11)?,r.get(12)?)))?;
                if !seen.insert(row.0.clone()){continue}
                // A canonical read rechecks the cache version under the same barrier.
                let (memory,latest)=self.read_memory(pkg,&row.0)?;if latest.version()!=row.5{continue}
                let original_sources:Vec<Value>=serde_json::from_str(&row.9)?;
                let (sources,sources_truncated)=source_summaries(&original_sources);
                items.push(json!({"id":row.0,"title":row.1,"summary":row.2,"applicability":row.3,"revision":row.4,"version":row.5,"game_version":row.6,"validation":row.7,"effective_validation":if self.source_review_required(pkg,&memory)?{"pending"}else{row.7.as_str()},"source_conflicts":self.source_conflicts(pkg,&memory)?,"status":row.8,"sources":sources,"source_count":original_sources.len(),"sources_truncated":sources_truncated,"source_details_reference":{"id":row.0,"revision":row.4},"section":row.10,"excerpt":row.11.chars().take(600).collect::<String>(),"chunk_id":chunk_id,"ordinal":row.12,"score":score,"reference":{"id":row.0,"revision":row.4,"version":row.5,"chunk_id":chunk_id,"section":row.10}}));
                if items.len()>=bounded_limit(args,5,20){break}
            }
            if query_vector.is_some()&&!semantic_used&&degraded.is_none(){degraded=Some(json!({"code":"semantic_index_pending_or_empty","detail":"当前过滤条件下暂无可用语义片段"}));}
            Ok(json!({"items":items,"retrieval":{"keyword":mode!="vector","semantic":semantic_used,"mode":if mode=="hybrid"&&semantic_used{"hybrid"}else if mode=="vector"{"vector"}else{"keyword"},"degraded_reason":degraded,"pending_vectors":status["pending_vectors"],"keyword_candidates":keyword.len(),"semantic_candidates":semantic.len()},"diagnostics":status["diagnostics"],"embedding_usage":usages}))
        })
    }
}
