use anyhow::{Context, Result};
use arrow::array::{Float32Array, RecordBatch, StringArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::connection::Connection;
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::{DistanceType, connect};
use log::debug;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

pub struct VectorDB {
    connection: Connection,
    table_name: String,
}

#[derive(Debug, Clone)]
pub struct ScoredPoint {
    pub id: String,
    pub score: f32,
    pub payload: serde_json::Value,
}

impl VectorDB {
    pub async fn new(path: PathBuf) -> Result<Self> {
        let uri = path.to_string_lossy().to_string();
        let connection = connect(&uri)
            .execute()
            .await
            .context("Failed to connect to LanceDB")?;
        Ok(Self {
            connection,
            table_name: "msrch_index".to_string(),
        })
    }

    pub async fn init_collection(&self, dim: usize) -> Result<()> {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    dim as i32,
                ),
                false,
            ),
            Field::new("file_path", DataType::Utf8, false),
            Field::new("chunk_index", DataType::UInt64, false),
            Field::new("content", DataType::Utf8, false),
            Field::new("context", DataType::Utf8, false),
        ]));

        // Check if table exists, if not create empty table
        let table_names = self.connection.table_names().execute().await?;
        if !table_names.contains(&self.table_name) {
            self.connection
                .create_empty_table(&self.table_name, schema)
                .execute()
                .await
                .context("Failed to create table")?;
        }

        Ok(())
    }

    pub async fn upsert_chunks(
        &self,
        chunks: Vec<(Uuid, Vec<f32>, serde_json::Value)>,
    ) -> Result<()> {
        if chunks.is_empty() {
            debug!("upsert_chunks: no chunks to upsert");
            return Ok(());
        }

        debug!("upsert_chunks: starting with {} chunks", chunks.len());

        let len = chunks.len();
        let mut ids = Vec::with_capacity(len);
        let mut vectors = Vec::with_capacity(len * 1024);
        let mut file_paths = Vec::with_capacity(len);
        let mut chunk_indices = Vec::with_capacity(len);
        let mut contents = Vec::with_capacity(len);
        let mut contexts = Vec::with_capacity(len);

        let dim = chunks[0].1.len();
        debug!("upsert_chunks: embedding dimension: {}", dim);

        for (id, vector, payload) in &chunks {
            ids.push(id.to_string());
            vectors.extend(vector);

            let obj = payload.as_object().unwrap();
            let file_path = obj.get("file_path").and_then(|v| v.as_str()).unwrap_or("");
            file_paths.push(file_path.to_string());
            let chunk_index = obj.get("chunk_index").and_then(|v| v.as_u64()).unwrap_or(0);
            chunk_indices.push(chunk_index);
            contents.push(
                obj.get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            );
            contexts.push(
                obj.get("context")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            );
        }

        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    dim as i32,
                ),
                false,
            ),
            Field::new("file_path", DataType::Utf8, false),
            Field::new("chunk_index", DataType::UInt64, false),
            Field::new("content", DataType::Utf8, false),
            Field::new("context", DataType::Utf8, false),
        ]));

        debug!("upsert_chunks: building RecordBatch");
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(ids)),
                Arc::new(arrow::array::FixedSizeListArray::from_iter_primitive::<
                    arrow::datatypes::Float32Type,
                    _,
                    _,
                >(
                    vectors
                        .chunks(dim)
                        .map(|c| Some(c.iter().copied().map(Some))),
                    dim as i32,
                )),
                Arc::new(StringArray::from(file_paths)),
                Arc::new(UInt64Array::from(chunk_indices)),
                Arc::new(StringArray::from(contents)),
                Arc::new(StringArray::from(contexts)),
            ],
        )?;
        debug!(
            "upsert_chunks: RecordBatch created with {} rows",
            batch.num_rows()
        );

        debug!("upsert_chunks: opening table '{}'", self.table_name);
        let table = self
            .connection
            .open_table(&self.table_name)
            .execute()
            .await?;

        // RecordBatch implements Scannable directly in lancedb 0.31.
        debug!("upsert_chunks: calling table.add()");
        table.add(batch).execute().await?;
        debug!("upsert_chunks: table.add() completed successfully");

        Ok(())
    }

    pub async fn count(&self) -> Result<usize> {
        let table_names = self.connection.table_names().execute().await?;
        if !table_names.contains(&self.table_name) {
            return Ok(0);
        }

        let table = self
            .connection
            .open_table(&self.table_name)
            .execute()
            .await?;
        let count = table.count_rows(None).await?;
        Ok(count)
    }

    pub async fn delete_by_ids(&self, ids: &[Uuid]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        let table_names = self.connection.table_names().execute().await?;
        if !table_names.contains(&self.table_name) {
            return Ok(());
        }

        let table = self
            .connection
            .open_table(&self.table_name)
            .execute()
            .await?;

        // Build a filter expression for all IDs: id IN ('id1', 'id2', ...)
        let id_strings: Vec<String> = ids.iter().map(|id| format!("'{}'", id)).collect();
        let filter = format!("id IN ({})", id_strings.join(", "));

        debug!(
            "delete_by_ids: deleting {} chunks with filter: {}",
            ids.len(),
            filter
        );
        table.delete(&filter).await?;
        debug!("delete_by_ids: deletion completed");

        Ok(())
    }

    pub async fn search(
        &self,
        vector: Vec<f32>,
        limit: u64,
        min_score: f32,
        filter: Option<&str>,
    ) -> Result<Vec<ScoredPoint>> {
        let table = self
            .connection
            .open_table(&self.table_name)
            .execute()
            .await?;

        let mut query = table
            .vector_search(vector)?
            .distance_type(DistanceType::Cosine)
            .limit(limit as usize);
        if let Some(predicate) = filter {
            query = query.only_if(predicate);
        }
        let results = query.execute().await?;

        let mut points = Vec::new();
        let mut stream = results;

        while let Some(batch) = stream.try_next().await? {
            let ids = batch
                .column_by_name("id")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let file_paths = batch
                .column_by_name("file_path")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let chunk_indices = batch
                .column_by_name("chunk_index")
                .unwrap()
                .as_any()
                .downcast_ref::<UInt64Array>()
                .unwrap();
            let contents = batch
                .column_by_name("content")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();

            let dists = if let Some(col) = batch.column_by_name("_distance") {
                col.as_any()
                    .downcast_ref::<Float32Array>()
                    .map(|a| a.values().to_vec())
            } else {
                None
            };
            let contexts = if let Some(col) = batch.column_by_name("context") {
                Some(col.as_any().downcast_ref::<StringArray>().unwrap())
            } else {
                None
            };

            for i in 0..batch.num_rows() {
                let id = ids.value(i).to_string();
                let file_path = file_paths.value(i).to_string();
                let chunk_index = chunk_indices.value(i);
                let content = contents.value(i).to_string();
                let context = contexts
                    .as_ref()
                    .map(|c| c.value(i).to_string())
                    .unwrap_or_default();
                let score = dists.as_ref().map(|d| 1.0 - d[i]).unwrap_or(0.0);

                if score >= min_score {
                    let payload = serde_json::json!({
                        "file_path": file_path,
                        "chunk_index": chunk_index,
                        "content": content,
                        "context": context
                    });

                    points.push(ScoredPoint { id, score, payload });
                }
            }
        }

        Ok(points)
    }
}
