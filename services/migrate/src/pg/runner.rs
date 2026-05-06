use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use sha2::{Digest, Sha256};
use sqlx::PgConnection;

use crate::error::MigrateError;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(sqlx::FromRow)]
struct AppliedRow {
    version: i32,
    checksum: String,
}

#[derive(Debug, Clone)]
pub struct MigrationFile {
    pub version: i32,
    pub description: String,
    pub checksum: String,
    pub sql: String,
}

impl MigrationFile {
    pub fn load(path: &Path) -> Result<Self, MigrateError> {
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| MigrateError::InvalidFilename(path.display().to_string()))?;

        let (prefix, rest) = filename
            .split_once('_')
            .ok_or_else(|| MigrateError::InvalidFilename(filename.to_string()))?;

        let version = prefix
            .parse::<i32>()
            .map_err(|_| MigrateError::InvalidFilename(filename.to_string()))?;

        let description = rest.trim_end_matches(".sql").replace('_', " ");
        let sql = std::fs::read_to_string(path)?;

        let mut hasher = Sha256::new();
        hasher.update(sql.as_bytes());
        let checksum = hex::encode(hasher.finalize());

        Ok(Self { version, description, checksum, sql })
    }
}

#[derive(Debug)]
pub struct MigrationStatus {
    pub version: i32,
    pub description: String,
    pub applied: bool,
}

pub struct Runner {
    schema: String,
    dir: PathBuf,
}

impl Runner {
    pub fn new(schema: &str, dir: &Path) -> Self {
        Self { schema: schema.to_owned(), dir: dir.to_owned() }
    }

    /// Bootstrap the schema and migration tracking table.
    ///
    /// Returns a `BoxFuture` (heap-allocated, type-erased, `Send`) so that the
    /// compiler does not need HRTB bounds when this future is awaited in a
    /// `Send`-constrained context (e.g., an axum handler).
    pub fn bootstrap<'c>(&self, conn: &'c mut PgConnection) -> BoxFuture<'c, Result<(), MigrateError>> {
        let create_schema = format!(r#"CREATE SCHEMA IF NOT EXISTS "{}""#, self.schema);
        let create_table = format!(
            r#"CREATE TABLE IF NOT EXISTS "{}".schema_migrations (
                version     INTEGER     PRIMARY KEY,
                description TEXT        NOT NULL,
                checksum    TEXT        NOT NULL,
                applied_at  TIMESTAMPTZ NOT NULL DEFAULT now()
            )"#,
            self.schema
        );
        Box::pin(async move {
            sqlx::query(&create_schema).execute(&mut *conn).await?;
            sqlx::query(&create_table).execute(&mut *conn).await?;
            Ok(())
        })
    }

    /// Apply all pending migrations.
    ///
    /// Returns a `BoxFuture` so that the caller (including axum handlers) can
    /// await it in a `Send` context without triggering HRTB `Executor<'_>` errors.
    pub fn apply_all<'c>(&self, conn: &'c mut PgConnection) -> BoxFuture<'c, Result<usize, MigrateError>> {
        let files_result = self.load_files().map(|mut f| { f.sort_by_key(|m| m.version); f });
        let schema = self.schema.clone();
        let query_applied_sql = format!(
            r#"SELECT version, checksum FROM "{}".schema_migrations ORDER BY version"#,
            self.schema
        );
        Box::pin(async move {
            let files = files_result?;
            let rows: Vec<AppliedRow> = sqlx::query_as(&query_applied_sql)
                .fetch_all(&mut *conn)
                .await?;
            let applied: HashMap<i32, String> =
                rows.into_iter().map(|r| (r.version, r.checksum)).collect();

            let mut count = 0usize;
            for file in files {
                match applied.get(&file.version) {
                    Some(stored) if stored != &file.checksum => {
                        return Err(MigrateError::ChecksumMismatch {
                            version: file.version,
                            stored: stored.clone(),
                            actual: file.checksum.clone(),
                        });
                    }
                    Some(_) => {}
                    None => {
                        apply_migration(conn, &schema, &file).await?;
                        count += 1;
                    }
                }
            }

            Ok(count)
        })
    }

    /// Return migration status for all known migrations.
    pub fn status<'c>(&self, conn: &'c mut PgConnection) -> BoxFuture<'c, Result<Vec<MigrationStatus>, MigrateError>> {
        let files_result = self.load_files().map(|mut f| { f.sort_by_key(|m| m.version); f });
        let query_applied_sql = format!(
            r#"SELECT version, checksum FROM "{}".schema_migrations ORDER BY version"#,
            self.schema
        );
        Box::pin(async move {
            let files = files_result?;
            let rows: Vec<AppliedRow> = sqlx::query_as(&query_applied_sql)
                .fetch_all(&mut *conn)
                .await?;
            let applied: HashMap<i32, String> =
                rows.into_iter().map(|r| (r.version, r.checksum)).collect();

            Ok(files
                .into_iter()
                .map(|f| MigrationStatus {
                    applied: applied.contains_key(&f.version),
                    version: f.version,
                    description: f.description,
                })
                .collect())
        })
    }

    fn load_files(&self) -> Result<Vec<MigrationFile>, MigrateError> {
        if !self.dir.exists() {
            return Err(MigrateError::MissingDir(self.dir.display().to_string()));
        }

        let mut files = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) == Some("sql") {
                files.push(MigrationFile::load(&path)?);
            }
        }
        Ok(files)
    }
}

/// Apply a single migration within an explicit transaction using raw SQL.
///
/// Uses `BEGIN`/`COMMIT`/`ROLLBACK` issued as raw SQL rather than sqlx's
/// `Transaction` type, which holds a `&mut PgConnection` borrow that creates a
/// self-referential state machine and triggers HRTB `Executor<'_>` errors when
/// the future needs to be `Send`.
fn apply_migration<'c>(
    conn: &'c mut PgConnection,
    schema: &str,
    file: &MigrationFile,
) -> BoxFuture<'c, Result<(), MigrateError>> {
    // Pre-compute all strings before the async block so no `&str` refs
    // into local vars are captured (avoids a secondary HRTB issue).
    let begin_tx = "BEGIN".to_owned();
    let set_search_path = format!(r#"SET LOCAL search_path TO "{schema}", public"#);
    let file_sql = file.sql.clone();
    let insert_sql = format!(
        r#"INSERT INTO "{schema}".schema_migrations (version, description, checksum)
           VALUES ($1, $2, $3)"#
    );
    let version = file.version;
    let description = file.description.clone();
    let checksum = file.checksum.clone();

    Box::pin(async move {
        sqlx::query(&begin_tx).execute(&mut *conn).await?;

        let result: Result<(), MigrateError> = async {
            // SET LOCAL is transaction-scoped; lets migration SQL omit schema prefix.
            sqlx::query(&set_search_path).execute(&mut *conn).await?;
            // sqlx::query does not support multi-statement strings, so split on ';'
            // while respecting string literals, identifiers, and comments.
            for stmt in split_statements(&file_sql) {
                sqlx::query(stmt).execute(&mut *conn).await?;
            }
            sqlx::query(&insert_sql)
                .bind(version)
                .bind(&description)
                .bind(&checksum)
                .execute(&mut *conn)
                .await?;
            sqlx::query("COMMIT").execute(&mut *conn).await?;
            Ok(())
        }
        .await;

        if let Err(e) = result {
            sqlx::query("ROLLBACK").execute(&mut *conn).await.ok();
            return Err(e);
        }
        Ok(())
    })
}

/// Split a SQL string into individual statements on unquoted `;` delimiters.
///
/// Handles single-quoted strings (`'...'`), double-quoted identifiers (`"..."`),
/// line comments (`-- ...`), block comments (`/* ... */`), and dollar-quoted
/// strings (`$tag$...$tag$`). Empty statements are omitted.
fn split_statements(sql: &str) -> Vec<&str> {
    let mut stmts = Vec::new();
    let bytes = sql.as_bytes();
    let len = bytes.len();
    let mut start = 0usize;
    let mut pos = 0usize;

    while pos < len {
        match bytes[pos] {
            // Line comment: skip to end of line
            b'-' if pos + 1 < len && bytes[pos + 1] == b'-' => {
                pos += 2;
                while pos < len && bytes[pos] != b'\n' {
                    pos += 1;
                }
            }
            // Block comment: skip to */
            b'/' if pos + 1 < len && bytes[pos + 1] == b'*' => {
                pos += 2;
                while pos + 1 < len && !(bytes[pos] == b'*' && bytes[pos + 1] == b'/') {
                    pos += 1;
                }
                if pos + 1 < len {
                    pos += 2;
                }
            }
            // Single-quoted string: handle '' escape
            b'\'' => {
                pos += 1;
                while pos < len {
                    if bytes[pos] == b'\'' {
                        pos += 1;
                        if pos < len && bytes[pos] == b'\'' {
                            pos += 1; // '' escape — continue
                        } else {
                            break;
                        }
                    } else {
                        pos += 1;
                    }
                }
            }
            // Double-quoted identifier: handle "" escape
            b'"' => {
                pos += 1;
                while pos < len {
                    if bytes[pos] == b'"' {
                        pos += 1;
                        if pos < len && bytes[pos] == b'"' {
                            pos += 1; // "" escape — continue
                        } else {
                            break;
                        }
                    } else {
                        pos += 1;
                    }
                }
            }
            // Dollar-quoted string: $tag$...$tag$
            b'$' => {
                let tag_start = pos;
                pos += 1;
                while pos < len && bytes[pos] != b'$' {
                    if bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_' {
                        pos += 1;
                    } else {
                        break; // not a valid dollar-quote tag
                    }
                }
                if pos < len && bytes[pos] == b'$' {
                    pos += 1;
                    let tag = &bytes[tag_start..pos];
                    let tag_len = tag.len();
                    while pos + tag_len <= len {
                        if &bytes[pos..pos + tag_len] == tag {
                            pos += tag_len;
                            break;
                        }
                        pos += 1;
                    }
                }
            }
            // Statement terminator
            b';' => {
                let stmt = sql[start..pos].trim();
                if !stmt.is_empty() {
                    stmts.push(stmt);
                }
                pos += 1;
                start = pos;
            }
            _ => {
                pos += 1;
            }
        }
    }

    // Trailing content with no terminating semicolon
    let trailing = sql[start..].trim();
    if !trailing.is_empty() {
        stmts.push(trailing);
    }

    stmts
}

#[cfg(test)]
mod tests {
    use super::split_statements;

    #[test]
    fn single_statement() {
        assert_eq!(split_statements("SELECT 1"), vec!["SELECT 1"]);
        assert_eq!(split_statements("SELECT 1;"), vec!["SELECT 1"]);
    }

    #[test]
    fn multiple_statements() {
        let sql = "CREATE TABLE foo (id INT);\nCREATE TABLE bar (id INT);";
        assert_eq!(split_statements(sql), vec!["CREATE TABLE foo (id INT)", "CREATE TABLE bar (id INT)"]);
    }

    #[test]
    fn ignores_semicolons_in_string_literals() {
        let sql = "INSERT INTO t VALUES ('hello; world');\nSELECT 1;";
        assert_eq!(split_statements(sql), vec!["INSERT INTO t VALUES ('hello; world')", "SELECT 1"]);
    }

    #[test]
    fn ignores_semicolons_in_line_comments() {
        let sql = "-- this; is a comment\nSELECT 1;";
        assert_eq!(split_statements(sql), vec!["-- this; is a comment\nSELECT 1"]);
    }

    #[test]
    fn ignores_semicolons_in_block_comments() {
        let sql = "/* semi; in comment */ SELECT 1;";
        assert_eq!(split_statements(sql), vec!["/* semi; in comment */ SELECT 1"]);
    }

    #[test]
    fn empty_and_whitespace_only_statements_skipped() {
        let sql = "  ;  SELECT 1;  ;  ";
        assert_eq!(split_statements(sql), vec!["SELECT 1"]);
    }

    #[test]
    fn dollar_quoted_string_with_semicolon() {
        let sql = "DO $$BEGIN RAISE NOTICE 'hi;'; END$$;\nSELECT 2;";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].starts_with("DO $$"));
        assert_eq!(stmts[1], "SELECT 2");
    }
}

/// Returns the path to the migrations directory for a given sub-dir name.
pub fn migrations_dir(base: &Path, subdir: &str) -> PathBuf {
    base.join("migrations").join(subdir)
}
