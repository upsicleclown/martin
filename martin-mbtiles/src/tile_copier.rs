extern crate core;

use crate::errors::MbtResult;
use crate::mbtiles::MbtType;
use crate::{MbtError, Mbtiles};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{query, Connection, Row, SqliteConnection};
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Clone, Default, Debug)]
pub struct TileCopierOptions {
    zooms: HashSet<u8>,
    min_zoom: Option<u8>,
    max_zoom: Option<u8>,
    //self.bbox = bbox
    verbose: bool,
}

#[derive(Clone, Debug)]
pub struct TileCopier {
    src_mbtiles: Mbtiles,
    dst_filepath: PathBuf,
    options: TileCopierOptions,
}

impl TileCopierOptions {
    pub fn new() -> Self {
        Self {
            zooms: HashSet::new(),
            min_zoom: None,
            max_zoom: None,
            verbose: false,
        }
    }

    pub fn zooms(mut self, zooms: Vec<u8>) -> Self {
        for zoom in zooms {
            self.zooms.insert(zoom);
        }
        self
    }

    pub fn min_zoom(mut self, min_zoom: u8) -> Self {
        self.min_zoom = Some(min_zoom);
        self
    }

    pub fn max_zoom(mut self, max_zoom: u8) -> Self {
        self.max_zoom = Some(max_zoom);
        self
    }

    pub fn verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }
}

impl TileCopier {
    pub fn new(
        src_filepath: PathBuf,
        dst_filepath: PathBuf,
        options: TileCopierOptions,
    ) -> MbtResult<Self> {
        Ok(TileCopier {
            src_mbtiles: Mbtiles::new(src_filepath)?,
            dst_filepath,
            options,
        })
    }

    pub async fn run(self) -> MbtResult<()> {
        let opt = SqliteConnectOptions::new()
            .read_only(true)
            .filename(PathBuf::from(&self.src_mbtiles.filepath()));
        let mut conn = SqliteConnection::connect_with(&opt).await?;
        let storage_type = self.src_mbtiles.detect_type(&mut conn).await?;

        let opt = SqliteConnectOptions::new()
            .create_if_missing(true)
            .filename(&self.dst_filepath);
        let mut conn = SqliteConnection::connect_with(&opt).await?;

        if query("SELECT 1 FROM sqlite_schema LIMIT 1")
            .fetch_optional(&mut conn)
            .await?
            .is_some()
        {
            return Err(MbtError::NonEmptyTargetFile(self.dst_filepath.clone()));
        }

        query("PRAGMA page_size = 512").execute(&mut conn).await?;
        query("VACUUM").execute(&mut conn).await?;

        query("ATTACH DATABASE ? AS sourceDb")
            .bind(self.src_mbtiles.filepath())
            .execute(&mut conn)
            .await?;

        let schema = query("SELECT sql FROM sourceDb.sqlite_schema WHERE tbl_name IN ('metadata', 'tiles', 'map', 'images')")
            .fetch_all(&mut conn)
            .await?;

        for row in &schema {
            let row: String = row.get(0);
            query(row.as_str()).execute(&mut conn).await?;
        }

        query("INSERT INTO metadata SELECT * FROM sourceDb.metadata")
            .execute(&mut conn)
            .await?;

        match storage_type {
            MbtType::TileTables => self.copy_tile_tables(&mut conn).await,
            MbtType::DeDuplicated => self.copy_deduplicated(&mut conn).await,
        }
    }

    async fn copy_tile_tables(&self, conn: &mut SqliteConnection) -> MbtResult<()> {
        // TODO: Handle options
        //  - bbox
        //  - verbose
        //  - zoom

        self.run_query_with_options(conn, "INSERT INTO tiles SELECT * FROM sourceDb.tiles")
            .await?;

        Ok(())
    }

    async fn copy_deduplicated(&self, conn: &mut SqliteConnection) -> MbtResult<()> {
        query("INSERT INTO map SELECT * FROM sourceDb.map")
            .execute(&mut *conn)
            .await?;

        self.run_query_with_options(
            conn,
            "INSERT INTO images
                SELECT images.tile_data, images.tile_id
                FROM sourceDb.images
                  JOIN sourceDb.map
                  ON images.tile_id = map.tile_id",
        )
        .await?;

        Ok(())
    }

    async fn run_query_with_options(
        &self,
        conn: &mut SqliteConnection,
        sql: &str,
    ) -> MbtResult<()> {
        let mut params: Vec<String> = vec![];

        let sql = if !&self.options.zooms.is_empty() {
            params.extend(self.options.zooms.iter().map(|z| z.to_string()));
            format!(
                "{sql} WHERE zoom_level IN ({})",
                vec!["?"; self.options.zooms.len()].join(",")
            )
        } else if let Some(min_zoom) = &self.options.min_zoom {
            if let Some(max_zoom) = &self.options.max_zoom {
                params.push(min_zoom.to_string());
                params.push(max_zoom.to_string());
                format!("{sql} WHERE zoom_level BETWEEN ? AND ?")
            } else {
                params.push(min_zoom.to_string());
                format!("{sql} WHERE zoom_level >= ?")
            }
        } else if let Some(max_zoom) = &self.options.max_zoom {
            params.push(max_zoom.to_string());
            format!("{sql} WHERE zoom_level <= ? ")
        } else {
            sql.to_string()
        };

        let mut query = query(sql.as_str());

        for param in params {
            query = query.bind(param);
        }

        query.execute(conn).await?;

        Ok(())
    }
}

// TODO: tests
// TODO: documentation
