//! Integration tests for OntologyManager CRUD operations.

use cognee_ontology::{OntologyError, OntologyManager};
use uuid::Uuid;

fn sample_turtle() -> Vec<u8> {
    br#"@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<http://example.org/Vehicle> a owl:Class ;
    rdfs:label "Vehicle" .
<http://example.org/Car> a owl:Class ;
    rdfs:subClassOf <http://example.org/Vehicle> ;
    rdfs:label "Car" .
"#
    .to_vec()
}

#[tokio::test]
async fn upload_and_list() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = OntologyManager::new(dir.path());
    let user = Uuid::new_v4();

    let meta = mgr
        .upload(
            user,
            "vehicles",
            "vehicles.ttl",
            &sample_turtle(),
            Some("Vehicle ontology"),
        )
        .await
        .unwrap();

    assert_eq!(meta.ontology_key, "vehicles");
    assert_eq!(meta.filename, "vehicles.ttl");
    assert_eq!(meta.size_bytes, sample_turtle().len() as u64);
    assert_eq!(meta.description.as_deref(), Some("Vehicle ontology"));

    let list = mgr.list(user).await.unwrap();
    assert_eq!(list.len(), 1);
    assert!(list.contains_key("vehicles"));
    assert_eq!(list["vehicles"].filename, "vehicles.ttl");
}

#[tokio::test]
async fn duplicate_key_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = OntologyManager::new(dir.path());
    let user = Uuid::new_v4();

    mgr.upload(user, "dup", "a.ttl", b"a", None).await.unwrap();
    let err = mgr
        .upload(user, "dup", "b.owl", b"b", None)
        .await
        .unwrap_err();
    assert!(matches!(err, OntologyError::DuplicateKey(_)));
}

#[tokio::test]
async fn get_contents_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = OntologyManager::new(dir.path());
    let user = Uuid::new_v4();
    let content = sample_turtle();

    mgr.upload(user, "rt", "schema.owl", &content, None)
        .await
        .unwrap();

    let retrieved = mgr.get_contents(user, "rt").await.unwrap();
    assert_eq!(retrieved, content);
}

#[tokio::test]
async fn delete_removes_file_and_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = OntologyManager::new(dir.path());
    let user = Uuid::new_v4();

    mgr.upload(user, "del", "s.ttl", b"x", None).await.unwrap();
    mgr.delete(user, "del").await.unwrap();

    let list = mgr.list(user).await.unwrap();
    assert!(list.is_empty());

    let err = mgr.get_contents(user, "del").await.unwrap_err();
    assert!(matches!(err, OntologyError::NotFound(_)));
}

#[tokio::test]
async fn format_validation() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = OntologyManager::new(dir.path());
    let user = Uuid::new_v4();

    // Rejected extensions
    for bad in ["data.txt", "data.csv", "data.json", "noext"] {
        let result = mgr.upload(user, "bad", bad, b"x", None).await;
        assert!(
            matches!(result, Err(OntologyError::InvalidFormat(_))),
            "Expected InvalidFormat for '{}', got {:?}",
            bad,
            result
        );
    }

    // Accepted extensions
    for (i, good) in ["a.owl", "b.ttl", "c.rdf", "d.xml", "e.nt", "f.jsonld"]
        .iter()
        .enumerate()
    {
        let key = format!("k{}", i);
        mgr.upload(user, &key, good, b"x", None).await.unwrap();
    }
    assert_eq!(mgr.list(user).await.unwrap().len(), 6);
}

#[tokio::test]
async fn build_resolver_loaded() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = OntologyManager::new(dir.path());
    let user = Uuid::new_v4();

    mgr.upload(user, "v", "vehicles.ttl", &sample_turtle(), None)
        .await
        .unwrap();

    let resolver = mgr.build_resolver(user).unwrap();
    assert!(resolver.is_loaded());
}

#[tokio::test]
async fn build_resolver_empty() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = OntologyManager::new(dir.path());
    let user = Uuid::new_v4();

    let resolver = mgr.build_resolver(user).unwrap();
    assert!(!resolver.is_loaded());
}

#[tokio::test]
async fn empty_state_operations() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = OntologyManager::new(dir.path());
    let user = Uuid::new_v4();

    assert!(mgr.list(user).await.unwrap().is_empty());

    let err = mgr.get_contents(user, "no-such-key").await.unwrap_err();
    assert!(matches!(err, OntologyError::NotFound(_)));

    let err = mgr.delete(user, "no-such-key").await.unwrap_err();
    assert!(matches!(err, OntologyError::NotFound(_)));
}

#[tokio::test]
async fn batch_upload() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = OntologyManager::new(dir.path());
    let user = Uuid::new_v4();

    let items = vec![
        (
            "k1".into(),
            "a.ttl".into(),
            b"aaa".to_vec(),
            Some("first".into()),
        ),
        ("k2".into(), "b.owl".into(), b"bbb".to_vec(), None),
        (
            "k3".into(),
            "c.rdf".into(),
            b"ccc".to_vec(),
            Some("third".into()),
        ),
    ];

    let results = mgr.upload_batch(user, items).await.unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].ontology_key, "k1");
    assert_eq!(results[1].ontology_key, "k2");
    assert_eq!(results[2].ontology_key, "k3");

    let list = mgr.list(user).await.unwrap();
    assert_eq!(list.len(), 3);
}

#[tokio::test]
async fn batch_upload_rejects_internal_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = OntologyManager::new(dir.path());
    let user = Uuid::new_v4();

    let items = vec![
        ("same".into(), "a.ttl".into(), b"x".to_vec(), None),
        ("same".into(), "b.owl".into(), b"y".to_vec(), None),
    ];
    let err = mgr.upload_batch(user, items).await.unwrap_err();
    assert!(matches!(err, OntologyError::DuplicateKey(_)));
}

#[tokio::test]
async fn user_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = OntologyManager::new(dir.path());
    let u1 = Uuid::new_v4();
    let u2 = Uuid::new_v4();

    mgr.upload(u1, "shared", "a.ttl", b"user1", None)
        .await
        .unwrap();
    mgr.upload(u2, "shared", "a.ttl", b"user2", None)
        .await
        .unwrap();

    assert_eq!(mgr.get_contents(u1, "shared").await.unwrap(), b"user1");
    assert_eq!(mgr.get_contents(u2, "shared").await.unwrap(), b"user2");

    // Deleting from user1 does not affect user2
    mgr.delete(u1, "shared").await.unwrap();
    assert!(mgr.list(u1).await.unwrap().is_empty());
    assert_eq!(mgr.list(u2).await.unwrap().len(), 1);
}

#[tokio::test]
async fn path_traversal_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = OntologyManager::new(dir.path());
    let user = Uuid::new_v4();

    for bad_key in ["../escape", "foo/bar", "foo\\bar", "..", ".", "a\0b", ""] {
        let result = mgr.upload(user, bad_key, "a.ttl", b"data", None).await;
        assert!(
            result.is_err(),
            "Expected error for key '{}', got Ok",
            bad_key.escape_debug()
        );
    }
}
