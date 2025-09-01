# 🦀 Cognee-RS (Rust Edition)

**Cognee-RS** is a Rust-based experimental framework for building **on-device AI memory**.  
It’s designed to run efficiently on constrained devices (smartwatch, phone) while still being portable to cloud infra when needed.

---

## ✨ Goals

- **On-device memory**: lightweight AI memory system that can run on devices like smartwatches (~1B parameter models) or phones (Phi-4 class).
- **Vector store first**:  
  - Uses a metastore and a vector database as the primary storage layer.  
  - No dedicated graph DB.
- **Transformations everywhere**:  
  - Each transformation stage can run **locally** or in the **cloud**.  
  - Cloud prep is not the immediate goal, but we’ll keep infra flexibility in mind.
- **Interoperability**: Integration with **BAML** for llm extraction.

---

## 🏗️ Architecture (high-level)

1. **Ingestion**  
   - Text, speech, or other data comes in.  
   - Converted into embeddings (device-friendly models where possible).

2. **Storage**  
   - Embeddings stored in collections (lightweight, efficient vector store).
   - Small, retrieved-based subgraphs for contextual reasoning — without a full graph DB.

3. **Retrieval + Reasoning**  
   - Memory is reconstructed through vector search.  
   - Subgraphs built dynamically during retrieval for contextual queries.

4. **Models**  
   - **Smartwatch tier** → ~1B parameter models.  
   - **Phone tier** → Models like **Phi-4**.  
   - Larger models (or fine-tuned variants) can run in the cloud.

---

## 🔧 Tech Stack

- **Rust** 🦀 — core implementation for speed and safety.  
- **Qdrant** — vector storage backend.
- **BAML** — llm model management.  
- **Local models** — small/efficient LLMs for on-device reasoning.  
- **Optional cloud bridge** — keep design modular so that transformations can be run remotely if needed.

---

## 🚦 Roadmap

- [ ] Core ingestion & embedding pipeline in Rust  
- [ ] Qdrant integration for vector storage  
- [ ] Retrieval 
- [ ] BAML bindings for orchestration  
- [ ] Device deployment profiles (watch / phone)  
- [ ] Optional cloud execution paths