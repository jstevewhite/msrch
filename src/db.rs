use anyhow::{Context, Result};
use arrow::array::{Float32Array, RecordBatch, RecordBatchIterator, StringArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::connection::Connection;
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::{connect, Table};
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
        let connection = connect(&uri).execute().await.context("Failed to connect to LanceDB")?;
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

    pub async fn upsert_chunks(&self, chunks: Vec<(Uuid, Vec<f32>, serde_json::Value)>) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        let len = chunks.len();
        let mut ids = Vec::with_capacity(len);
        let mut vectors = Vec::with_capacity(len * 1024); 
        let mut file_paths = Vec::with_capacity(len);
        let mut chunk_indices = Vec::with_capacity(len);
        let mut contents = Vec::with_capacity(len);

        let dim = chunks[0].1.len();

        for (id, vector, payload) in chunks {
            ids.push(id.to_string());
            vectors.extend(vector);
            
            let obj = payload.as_object().unwrap();
            file_paths.push(obj.get("file_path").and_then(|v| v.as_str()).unwrap_or("").to_string());
            chunk_indices.push(obj.get("chunk_index").and_then(|v| v.as_u64()).unwrap_or(0));
            contents.push(obj.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string());
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
        ]));

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(ids)),
                Arc::new(arrow::array::FixedSizeListArray::from_iter_primitive::<arrow::datatypes::Float32Type, _, _>(
                    vectors.chunks(dim).map(|c| Some(c.iter().copied().map(Some)))
                    , dim as i32
                )),
                Arc::new(StringArray::from(file_paths)),
                Arc::new(UInt64Array::from(chunk_indices)),
                Arc::new(StringArray::from(contents)),
            ],
        )?;

        let table = self.connection.open_table(&self.table_name).execute().await?;
        
        // Wrap in RecordBatchIterator to satisfy IntoArrow
        let reader = RecordBatchIterator::new(vec![Ok(batch)], schema);
        table.add(reader).execute().await?;

        Ok(())
    }

    pub async fn search(&self, vector: Vec<f32>, limit: u64, _min_score: f32) -> Result<Vec<ScoredPoint>> {
        let table = self.connection.open_table(&self.table_name).execute().await?;
        
        let results = table
            .vector_search(vector)?
            .limit(limit as usize)
            .execute()
            .await?;
            
        let mut points = Vec::new();
        let mut stream = results;
        
        while let Some(batch) = stream.try_next().await? {
            let ids = batch.column_by_name("id").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let file_paths = batch.column_by_name("file_path").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let chunk_indices = batch.column_by_name("chunk_index").unwrap().as_any().downcast_ref::<UInt64Array>().unwrap();
            let contents = batch.column_by_name("content").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            
            let dists = if let Some(col) = batch.column_by_name("_distance") {
                 col.as_any().downcast_ref::<Float32Array>().map(|a| a.values().to_vec())
            } else {
                None
            };

            for i in 0..batch.num_rows() {
                let id = ids.value(i).to_string();
                let file_path = file_paths.value(i).to_string();
                let chunk_index = chunk_indices.value(i);
                let content = contents.value(i).to_string();
                let score = dists.as_ref().map(|d| 1.0 - d[i]).unwrap_or(0.0);

                let payload = serde_json::json!({
                    "file_path": file_path,
                    "chunk_index": chunk_index,
                    "content": content
                });

                points.push(ScoredPoint {
                    id,
                    score,
                    payload,
                });
            }
        }

        Ok(points)
    }
}
