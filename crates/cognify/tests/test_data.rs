#![allow(dead_code)]

pub const TEST_TEXT_TECHCORP: &str = r#"
Alice Johnson is a software engineer at TechCorp, a technology company based in San Francisco, California. 
She has been working there for five years, specializing in machine learning and artificial intelligence.

Bob Smith, the CEO of TechCorp, founded the company in 2010 with a vision to revolutionize how businesses 
use data. Under his leadership, TechCorp has grown from a small startup to a company with over 500 employees.

The company's headquarters is located in the heart of San Francisco's financial district, occupying three 
floors of a modern office building. TechCorp also has satellite offices in New York City and Austin, Texas.

Last month, Alice presented her latest project at the AI Conference in Seattle, Washington. Her work on 
improving natural language processing models received significant attention from industry experts. She 
collaborated with Dr. Emma Chen from Stanford University on this research.

TechCorp recently announced a partnership with DataSystems Inc., another major player in the technology sector. 
This partnership aims to integrate TechCorp's AI capabilities with DataSystems' cloud infrastructure platform.
"#;

pub const TEST_TEXT_RESEARCH: &str = r#"
Dr. Maria Rodriguez leads the Quantum Computing Laboratory at MIT, where she has been conducting groundbreaking 
research on quantum error correction since 2018. Her team consists of twelve researchers from various countries, 
including Dr. James Lee from South Korea and Dr. Fatima Abbas from Egypt.

The laboratory is funded by a $10 million grant from the National Science Foundation, which was awarded in 2020. 
This funding has enabled the acquisition of state-of-the-art quantum computers manufactured by QuantumTech Industries, 
a Canadian company specializing in quantum hardware.

Dr. Rodriguez recently published a paper in Nature Physics, co-authored with Professor Chen Wei from Tsinghua 
University in Beijing. The research demonstrates a novel approach to reducing quantum decoherence, which could 
significantly improve the reliability of quantum computers.

The MIT laboratory collaborates with several institutions worldwide, including Cambridge University in the UK, 
the Max Planck Institute in Germany, and RIKEN in Japan. These partnerships facilitate the exchange of ideas 
and resources in the rapidly evolving field of quantum computing.
"#;

pub const TEST_TEXT_ARTICLE: &str = r#"
Artificial intelligence has made remarkable progress over the past decade, transforming 
industries ranging from healthcare to transportation. Machine learning algorithms can now 
diagnose diseases with accuracy rivaling human experts, while autonomous vehicles are 
becoming a reality on roads worldwide. Natural language processing models have achieved 
unprecedented capabilities in understanding and generating human language.

The development of large language models, particularly transformer-based architectures, 
has been a key driver of this progress. These models can perform a wide variety of tasks, 
from translation and summarization to code generation and creative writing. Their ability 
to learn from vast amounts of data has enabled them to capture complex patterns in language 
and knowledge.

However, these advances also raise important ethical considerations. Issues of bias, privacy, 
and the environmental impact of training large models have become increasingly prominent in 
academic and public discourse. Researchers and policymakers are working to develop frameworks 
that ensure AI systems are developed and deployed responsibly.

Looking ahead, the integration of AI into everyday life will continue to accelerate. Edge 
computing and more efficient model architectures will enable AI capabilities on personal 
devices, while advances in multi-modal learning will allow systems to understand and generate 
content across text, images, and audio simultaneously.
"#;

pub const TEST_TEXT_SHORT: &str = r#"
Quantum computing represents a paradigm shift in computation. Unlike classical computers 
that use bits, quantum computers use qubits that can exist in superposition states. This 
allows them to solve certain problems exponentially faster than traditional computers.
"#;

pub const TEST_TEXT_EMBEDDINGS_BASIC: &str = "TechCorp is an organization based in San Francisco. Alice works at TechCorp as a software engineer.";

pub const TEST_TEXT_EMBEDDINGS_ENTITY: &str = "TechCorp is a technology company founded in 2020. Alice is the CEO of TechCorp and works in San Francisco. Bob is a software engineer at TechCorp.";

pub const TEST_TEXT_EMBEDDINGS_TRIPLETS_DEFAULT: &str =
    "Alice works at TechCorp. Bob also works at TechCorp.";
