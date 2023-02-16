//! Code for supporting the FASTA directory access.

use std::path::{Path, PathBuf};

use chrono::NaiveDateTime;
use noodles::core::{Position, Region};
use rusqlite::{Connection, OpenFlags};

static EXPECTED_SCHEMA_VERSION: u32 = 1;

/// A record from the `db.sqlite3` database.
#[derive(Debug, PartialEq)]
pub struct SeqInfoRecord {
    pub seq_id: String,
    pub len: usize,
    pub alpha: String,
    pub added: NaiveDateTime,
    pub relpath: String,
}

/// This class provides a simple key-value interface to a directory of compressed FASTA files.
///
/// Sequences are stored in dated FASTA files.  Dating the files enables compact storage with
/// multiple releases (using hard links) and efficient incremental updtes and transfers (e.g.,
/// via rsync).  The FASTA files are compressed with block gzip, enabling fast random access
/// to arbitrary regions of even large (chromosome-sized) sequences.
///
/// When the key is a hash based on sequence (e.g., SHA512), the combination provides a
/// convenient non-redundant storage of sequences with fast access to sequences and sequence
/// slices, compact storage and easy replication.
#[derive(Debug)]
pub struct FastaDir {
    /// The path to the directory ("$instance/sequences" within seqrepo).
    root_dir: PathBuf,
    /// Connection to the SQLite database "db.sqlite3" inside root_dir.
    conn: Connection,
    /// Schema version.
    schema_version: u32,
}

impl FastaDir {
    /// Initialize new `FastaDir`, will open connection to the database.
    pub fn new<P>(root_dir: P) -> Result<Self, anyhow::Error>
    where
        P: AsRef<Path>,
    {
        let root_dir = PathBuf::from(root_dir.as_ref());

        let db_path = root_dir.join("db.sqlite3");
        let conn = Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;

        let schema_version = Self::fetch_schema_version(&conn)?;
        if schema_version != EXPECTED_SCHEMA_VERSION {
            Err(anyhow::anyhow!(
                "Upgrade required: Database schema version is {} and the code \
                expects {}",
                schema_version,
                EXPECTED_SCHEMA_VERSION
            ))
        } else {
            Ok(FastaDir {
                root_dir,
                conn,
                schema_version,
            })
        }
    }

    /// Load schema version from the database.
    fn fetch_schema_version(conn: &Connection) -> Result<u32, anyhow::Error> {
        let sql = "select value from meta where key = 'schema version'";
        let mut stmt = conn.prepare(sql)?;

        Ok(stmt.query_row([], |row| {
            let value: String = row.get(0).unwrap();
            Ok(str::parse::<u32>(&value).unwrap())
        })?)
    }

    /// Schema version as read from the database.
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Load `SeqInfoRecord` from database.
    pub fn fetch_seqinfo(&self, seq_id: &str) -> Result<SeqInfoRecord, anyhow::Error> {
        let sql = "select seq_id, len, alpha, added, relpath from seqinfo \
        where seq_id = ? order by added desc";
        let mut stmt = self.conn.prepare(sql)?;

        Ok(stmt.query_row([&seq_id], |row| {
            let added: String = row.get(3)?;
            println!("{}", &added);
            let added = NaiveDateTime::parse_from_str(&added, "%Y-%m-%d %H:%M:%S")
                .expect("could not convert timestamp");

            Ok(SeqInfoRecord {
                seq_id: row.get(0)?,
                len: row.get(1)?,
                alpha: row.get(2)?,
                added,
                relpath: row.get(4)?,
            })
        })?)
    }

    /// Load complete sequence from FASTA directory.
    pub fn fetch_sequence(&self, seq_id: &str) -> Result<String, anyhow::Error> {
        self.fetch_sequence_part(seq_id, None, None)
    }

    /// Load sequence fragment from FASTA directory.
    pub fn fetch_sequence_part(
        &self,
        seq_id: &str,
        begin: Option<usize>,
        end: Option<usize>,
    ) -> Result<String, anyhow::Error> {
        let seqinfo = self.fetch_seqinfo(seq_id)?;

        let path_bgzip = self.root_dir.join(seqinfo.relpath);
        let path_bgzip = path_bgzip.as_path().to_str().unwrap();

        let bgzf_index = noodles::bgzf::gzi::read(format!("{path_bgzip}.gzi"))?;
        let bgzf_reader = noodles::bgzf::indexed_reader::Builder::default()
            .set_index(bgzf_index)
            .build_from_path(path_bgzip)?;
        let fai_index = noodles::fasta::fai::read(format!("{path_bgzip}.fai"))?;
        let mut fai_reader = noodles::fasta::indexed_reader::Builder::default()
            .set_index(fai_index)
            .build_from_reader(bgzf_reader)?;

        let start = Position::try_from(begin.map(|start| start + 1).unwrap_or(1))?;
        let end = Position::try_from(
            end.map(|end| std::cmp::min(end, seqinfo.len))
                .unwrap_or(seqinfo.len),
        )?;
        let region = Region::new(seq_id, start..=end);

        let record = fai_reader.query(&region)?;

        Ok(std::str::from_utf8(record.sequence().as_ref())
            .unwrap()
            .to_string())
    }
}

#[cfg(test)]
mod test {
    use super::FastaDir;

    use pretty_assertions::assert_eq;

    #[test]
    fn smoke() -> Result<(), anyhow::Error> {
        let fd = FastaDir::new("tests/data/seqrepo/latest/sequences")?;
        assert_eq!(fd.schema_version(), 1);

        Ok(())
    }

    #[test]
    fn fetch_seqinfo() -> Result<(), anyhow::Error> {
        let fd = FastaDir::new("tests/data/seqrepo/latest/sequences")?;
        let seq_id = "5q5HZTCRudL17NTiv5Bn6th__0FrZH04";
        let si = fd.fetch_seqinfo(seq_id)?;
        assert_eq!(
            format!("{:?}", &si),
            "SeqInfoRecord { seq_id: \"5q5HZTCRudL17NTiv5Bn6th__0FrZH04\", len: 1873, \
            alpha: \"ACGT\", added: 2023-02-16T09:46:06, \
            relpath: \"2023/0216/0946/1676540766.9148078.fa.bgz\" }",
        );

        Ok(())
    }

    #[test]
    fn fetch_sequence() -> Result<(), anyhow::Error> {
        let fd = FastaDir::new("tests/data/seqrepo/latest/sequences")?;
        let seq_id = "5q5HZTCRudL17NTiv5Bn6th__0FrZH04";

        assert_eq!(
            fd.fetch_sequence(seq_id)?,
            "ACTGCTGAGCTGGGAGATGTCGGCGGCGTGTTGGGAGGAACCGTGGGGTCTTCCCGGCGGCTTT\
            GCGAAGCGGGTCCTGGTGACCGGCGGTGCTGGTTTCATGTAGGTAATGGCGCCGCTAGCCAAGCA\
            GTGGCTCCCCAGAAACCCCTACCTTTTCCCGCAGCTCTGCTTGCCCTAGTGCATCACATATGATT\
            GTCTCTTTAGTGGAAGATTATCCAAACTATATGATCATAAATCTAGACAAGCTGGATTACTGTGC\
            AAGCTTGAAGAATCTTGAAACCATTTCTAACAAACAGAACTACAAATTTATACAGGGTGACATAT\
            GTGATTCTCACTTTGTGAAACTGCTTTTTGAAACAGAGAAAATAGATATAGTACTACATTTTGCC\
            GCACAAACACATGTAGATCTTTCATTCGTACGTGCCTTTGAGTTTACCTATGTTAATGTTTATGG\
            CACTCACGTTTTGGTAAGTGCTGCTCATGAAGCCAGAGTGGAGAAGTTTATTTATGTCAGCACAG\
            ATGAAGTATATGGTGGCAGTCTTGATAAGGAATTTGATGAATCTTCACCCAAACAACCTACAAAT\
            CCTTATGCATCATCTAAAGCAGCTGCTGAATGTTTTGTACAGTCTTACTGGGAACAATATAAGTT\
            TCCAGTTGTCATCACAAGAAGCAGTAATGTTTATGGACCACATCAATATCCAGAAAAGGTTATTC\
            CAAAATTTATATCTTTGCTACAGCACAACAGGAAATGTTGCATTCATGGGTCAGGGCTTCAAACA\
            AGAAACTTCCTTTATGCTACTGATGTTGTAGAAGCATTTCTCACTGTCCTCAAAAAAGGGAAACC\
            AGGTGAAATTTATAACATCGGAACCAATTTTGAAATGTCAGTTGTCCAGCTTGCCAAAGAACTAA\
            TACAACTGATCAAAGAGACCAATTCAGAGTCTGAAATGGAAAATTGGGTTGATTATGTTAATGAT\
            AGACCCACCAATGACATGAGATACCCAATGAAGTCAGAAAAAATACATGGCTTAGGATGGAGACC\
            TAAAGTGCCTTGGAAAGAAGGAATAAAGAAAACAATTGAATGGTACAGAGAGAATTTTCACAACT\
            GGAAGAATGTGGAAAAGGCATTAGAACCCTTTCCGGTATAATCACCATTTATATAGTCGAGACAG\
            TTGTCAAAGAAGAAAGTTATCCTACCTCGCCAAGTGGTATGAAATTAAGTGACCAAATGAAGTGC\
            ACTCTTTTCTTTTGGAATTAGATTCATGACTTTCTGTATAAAATTCAAATGCAGAATGCCTCAAT\
            CTTTGGGAGAGTTTCAGTACTGGCATAGAATTTAAATGTCAAAATTCTTTCTGAAACCCTTTCTC\
            CTAGAAACTAGGAAATAATAGGTGTAGAAGACTCTCCCTAAGGGTAGCCAGGAAGAAGTCTCCTG\
            ATTCGGACAACCATGAGGGGTAGTGGTGCTAGGGAGAAGGCAACCTTCACTGGTTTTGAACTCAG\
            TGCCTAAGAAAGTCTCTGAAATGTTCGTTTTTAGGCAATATAGGATGTCTTAGGCCCTAATTCAC\
            CATTTCTTTTTTAAGATCTGATATGCTATCATTGCCTTAATAATGGAACAAAATAGAAGCATATC\
            TAACACTTTTTAAATTGATAATTTTGTAAAATTGATTACGTTGAATGCTTTTTAAGAGAAGTGTG\
            TAAAGTTTTTATATTTTCACAATTAACGTATGTAAAACCTTGTATCAGAAATTTATCATGTTTAC\
            TGTTTAAAATGATTGTATTTATAAAATTGTCAATATCTTAATGTATTTAATGTAGAATATTGCTT\
            TTTAAAATAATGTTTTTATTTTGCTGTAGAAAAATAAAAAAAAATTTGATTATA"
        );

        Ok(())
    }

    #[test]
    fn fetch_sequence_part() -> Result<(), anyhow::Error> {
        let fd = FastaDir::new("tests/data/seqrepo/latest/sequences")?;
        let seq_id = "5q5HZTCRudL17NTiv5Bn6th__0FrZH04";

        assert_eq!(
            fd.fetch_sequence_part(seq_id, Some(0), Some(10))?,
            "ACTGCTGAGC"
        );
        assert_eq!(
            fd.fetch_sequence_part(seq_id, Some(100), Some(110))?,
            "ATGTAGGTAA"
        );

        Ok(())
    }
}