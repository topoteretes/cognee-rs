use std::error::Error;
use std::path::Path;

use lbug::{Connection, Database, SystemConfig};

const DATA_DIR: &str = "./ladybug-data";

fn main() -> Result<(), Box<dyn Error>> {
    let data_path = Path::new(DATA_DIR);
    if data_path.is_dir() {
        std::fs::remove_dir_all(data_path)?;
    } else if data_path.exists() {
        std::fs::remove_file(data_path)?;
    }

    println!("---- Initialize LadybugDB ----");
    let db = Database::new(DATA_DIR, SystemConfig::default())?;
    let conn = Connection::new(&db)?;

    conn.query("INSTALL json")?;
    conn.query("LOAD EXTENSION json")?;

    println!("---- Create Schema ----");
    conn.query(
        "CREATE NODE TABLE IF NOT EXISTS Node(
            id STRING PRIMARY KEY,
            name STRING,
            type STRING,
            created_at TIMESTAMP,
            updated_at TIMESTAMP,
            properties STRING
        )",
    )?;
    conn.query(
        "CREATE REL TABLE IF NOT EXISTS EDGE(
            FROM Node TO Node,
            relationship_name STRING,
            created_at TIMESTAMP,
            updated_at TIMESTAMP,
            properties STRING
        )",
    )?;

    println!("---- Insert Nodes ----");
    conn.query(
        "CREATE (:Node {
            id: 'person-1',
            name: 'Alice',
            type: 'Person',
            created_at: timestamp('2024-01-15 10:30:00'),
            updated_at: timestamp('2024-06-01 08:00:00'),
            properties: '{\"location\": \"Berlin\", \"occupation\": \"Engineer\", \"languages\": \"Rust,Python\"}'
        })",
    )?;
    conn.query(
        "CREATE (:Node {
            id: 'person-2',
            name: 'Bob',
            type: 'Person',
            created_at: timestamp('2024-02-20 14:00:00'),
            updated_at: timestamp('2024-05-10 09:15:00'),
            properties: '{\"location\": \"London\", \"occupation\": \"Researcher\", \"languages\": \"Python,Java\"}'
        })",
    )?;
    conn.query(
        "CREATE (:Node {
            id: 'person-3',
            name: 'Charlie',
            type: 'Person',
            created_at: timestamp('2024-03-05 11:45:00'),
            updated_at: timestamp('2024-07-20 16:30:00'),
            properties: '{\"location\": \"Berlin\", \"occupation\": \"Data Scientist\", \"languages\": \"Python,R\"}'
        })",
    )?;
    conn.query(
        "CREATE (:Node {
            id: 'org-1',
            name: 'Cognee',
            type: 'Organization',
            created_at: timestamp('2023-06-01 09:00:00'),
            updated_at: timestamp('2024-08-01 12:00:00'),
            properties: '{\"location\": \"Berlin\", \"domain\": \"AI\", \"size\": \"startup\"}'
        })",
    )?;
    conn.query(
        "CREATE (:Node {
            id: 'project-1',
            name: 'Knowledge Engine',
            type: 'Project',
            created_at: timestamp('2024-01-01 00:00:00'),
            updated_at: timestamp('2024-09-15 10:00:00'),
            properties: '{\"status\": \"active\", \"language\": \"Rust\", \"repository\": \"github.com/cognee/engine\"}'
        })",
    )?;
    println!("Inserted 5 nodes (3 Person, 1 Organization, 1 Project)");

    println!("---- Insert Edges ----");
    conn.query(
        "MATCH (a:Node {id: 'person-1'}), (b:Node {id: 'person-2'})
            CREATE (a)-[:EDGE {
                relationship_name: 'KNOWS',
                created_at: timestamp('2024-03-01 10:00:00'),
                updated_at: timestamp('2024-03-01 10:00:00'),
                properties: '{\"context\": \"conference\", \"strength\": \"strong\"}'
            }]->(b)",
    )?;
    conn.query(
        "MATCH (a:Node {id: 'person-2'}), (b:Node {id: 'person-3'})
            CREATE (a)-[:EDGE {
                relationship_name: 'KNOWS',
                created_at: timestamp('2024-04-15 14:00:00'),
                updated_at: timestamp('2024-04-15 14:00:00'),
                properties: '{\"context\": \"university\", \"strength\": \"moderate\"}'
            }]->(b)",
    )?;
    conn.query(
        "MATCH (a:Node {id: 'person-1'}), (b:Node {id: 'org-1'})
            CREATE (a)-[:EDGE {
                relationship_name: 'WORKS_AT',
                created_at: timestamp('2024-01-15 10:30:00'),
                updated_at: timestamp('2024-01-15 10:30:00'),
                properties: '{\"role\": \"Senior Engineer\", \"department\": \"Core\"}'
            }]->(b)",
    )?;
    conn.query(
        "MATCH (a:Node {id: 'person-3'}), (b:Node {id: 'org-1'})
            CREATE (a)-[:EDGE {
                relationship_name: 'WORKS_AT',
                created_at: timestamp('2024-03-05 11:45:00'),
                updated_at: timestamp('2024-03-05 11:45:00'),
                properties: '{\"role\": \"Data Scientist\", \"department\": \"Research\"}'
            }]->(b)",
    )?;
    conn.query(
        "MATCH (a:Node {id: 'org-1'}), (b:Node {id: 'project-1'})
            CREATE (a)-[:EDGE {
                relationship_name: 'OWNS',
                created_at: timestamp('2024-01-01 00:00:00'),
                updated_at: timestamp('2024-01-01 00:00:00'),
                properties: '{\"priority\": \"high\"}'
            }]->(b)",
    )?;
    println!("Inserted 5 edges");

    println!("\n---- Query: All Person nodes ----");
    let result =
        conn.query("MATCH (n:Node) WHERE n.type = 'Person' RETURN n.id, n.name, n.properties")?;
    println!("{}", result);

    println!("---- Query: People located in Berlin (json_extract) ----");
    let result = conn.query(
        "MATCH (n:Node)
            WHERE n.type = 'Person'
            AND json_extract(n.properties, 'location') = '\"Berlin\"'
            RETURN n.name, json_extract(n.properties, 'location') AS location,
                json_extract(n.properties, 'occupation') AS occupation",
    )?;
    println!("{}", result);

    println!("---- Query: Who knows whom? ----");
    let result = conn.query(
        "MATCH (a:Node)-[e:EDGE]->(b:Node)
            WHERE e.relationship_name = 'KNOWS'
            RETURN a.name AS from, b.name AS to,
                json_extract(e.properties, 'context') AS context",
    )?;
    println!("{}", result);

    println!("---- Query: 2-hop KNOWS path from Alice ----");
    let result = conn.query(
        "MATCH (a:Node)-[e1:EDGE]->(b:Node)-[e2:EDGE]->(c:Node)
            WHERE a.name = 'Alice'
            AND e1.relationship_name = 'KNOWS'
            AND e2.relationship_name = 'KNOWS'
            RETURN a.name AS from, b.name AS via, c.name AS to",
    )?;
    println!("{}", result);

    println!("---- Query: Cognee employees and roles ----");
    let result = conn.query(
        "MATCH (p:Node)-[e:EDGE]->(o:Node)
            WHERE e.relationship_name = 'WORKS_AT' AND o.name = 'Cognee'
            RETURN p.name AS person,
                json_extract(e.properties, 'role') AS role,
                json_extract(e.properties, 'department') AS department",
    )?;
    println!("{}", result);

    println!("---- Query: Nodes sharing location 'Berlin' ----");
    let result = conn.query(
        "MATCH (a:Node), (b:Node)
            WHERE a.id < b.id
            AND json_extract(a.properties, 'location') = '\"Berlin\"'
            AND json_extract(b.properties, 'location') = '\"Berlin\"'
            RETURN a.name AS node_a, a.type AS type_a, b.name AS node_b, b.type AS type_b",
    )?;
    println!("{}", result);

    if data_path.is_dir() {
        std::fs::remove_dir_all(data_path)?;
    } else if data_path.exists() {
        std::fs::remove_file(data_path)?;
    }
    println!("---- Done ----");

    Ok(())
}
