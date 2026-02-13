//! Payload system for multi-stage async processing pipelines
//!
//! This module provides a flexible, trait-based payload container system for managing
//! data flow through multi-stage processing pipelines.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use uuid::Uuid;

// ============================================================================
// PayloadBase - Metadata container
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayloadMetaInfo {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
}

impl Default for PayloadMetaInfo {
    fn default() -> Self {
        Self::new()
    }
}

impl PayloadMetaInfo {
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            created_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayloadBase {
    pub metainfo: PayloadMetaInfo,
}

impl Default for PayloadBase {
    fn default() -> Self {
        Self::new()
    }
}

impl PayloadBase {
    pub fn new() -> Self {
        Self {
            metainfo: PayloadMetaInfo::new(),
        }
    }
}

// ============================================================================
// PropertyStatus - State tracking for payload properties
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyStatus {
    Empty,
    Processing,
    Done,
    Errored(String),
}

// ============================================================================
// PayloadTrait - Core trait for all payload types
// ============================================================================

#[allow(dead_code)]
pub trait PayloadTrait: Send + Sync + 'static {
    fn payload_id(&self) -> Uuid;
    fn payload_get_property_status(&self, property: &str) -> Option<PropertyStatus>;
    fn payload_set_property_status(&self, property: &str, status: PropertyStatus);
    fn payload_get_arc(
        &self,
        property: &str,
    ) -> Result<Box<dyn std::any::Any + Send + Sync>, String>;
    fn payload_get_copy(
        &self,
        property: &str,
    ) -> Result<Box<dyn std::any::Any + Send + Sync>, String>;
    fn payload_get_all_property_statuses(&self) -> HashMap<String, PropertyStatus>;
}

pub trait PayloadConstructor: PayloadTrait {
    fn new(chunks: Vec<Arc<String>>) -> Self;
}

// ============================================================================
// CogneePayload - Generic payload implementation with 2 result fields
// ============================================================================

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CogneePayload<TC, T1, T2>
where
    TC: Clone + Send + Sync + 'static,
    T1: Clone + Send + Sync + 'static,
    T2: Clone + Send + Sync + 'static,
{
    base: Arc<RwLock<PayloadBase>>,
    chunks: Arc<RwLock<Vec<Arc<TC>>>>,
    result1: Arc<RwLock<Vec<Arc<T1>>>>,
    result2: Arc<RwLock<Vec<Arc<T2>>>>,
    property_status: Arc<Mutex<HashMap<String, PropertyStatus>>>,
}

#[allow(dead_code)]
impl<TC, T1, T2> CogneePayload<TC, T1, T2>
where
    TC: Clone + Send + Sync + 'static,
    T1: Clone + Send + Sync + 'static,
    T2: Clone + Send + Sync + 'static,
{
    pub fn new(chunks: Vec<Arc<TC>>) -> Self {
        let mut status = HashMap::new();
        status.insert("base".to_string(), PropertyStatus::Done);
        status.insert(
            "chunks".to_string(),
            if chunks.is_empty() {
                PropertyStatus::Empty
            } else {
                PropertyStatus::Done
            },
        );
        status.insert("result1".to_string(), PropertyStatus::Empty);
        status.insert("result2".to_string(), PropertyStatus::Empty);

        Self {
            base: Arc::new(RwLock::new(PayloadBase::new())),
            chunks: Arc::new(RwLock::new(chunks)),
            result1: Arc::new(RwLock::new(Vec::new())),
            result2: Arc::new(RwLock::new(Vec::new())),
            property_status: Arc::new(Mutex::new(status)),
        }
    }

    pub fn get_arc(&self, property: &str) -> Result<Box<dyn std::any::Any + Send + Sync>, String> {
        match property {
            "chunks" => Ok(Box::new(Arc::clone(&self.chunks))),
            "result1" => Ok(Box::new(Arc::clone(&self.result1))),
            "result2" => Ok(Box::new(Arc::clone(&self.result2))),
            "property_status" => Ok(Box::new(Arc::clone(&self.property_status))),
            _ => Err(format!("Unknown property: {property}")),
        }
    }

    pub fn get_copy(&self, property: &str) -> Result<Box<dyn std::any::Any + Send + Sync>, String> {
        match property {
            "chunks" => {
                let chunks = self.chunks.read().unwrap();
                Ok(Box::new(chunks.clone()))
            }
            "result1" => {
                let result1 = self.result1.read().unwrap();
                Ok(Box::new(result1.clone()))
            }
            "result2" => {
                let result2 = self.result2.read().unwrap();
                Ok(Box::new(result2.clone()))
            }
            _ => Err(format!("Unknown property: {property}")),
        }
    }

    pub fn get_property_status(&self, property: &str) -> Option<PropertyStatus> {
        let status = self.property_status.lock().unwrap();
        status.get(property).cloned()
    }

    pub fn set_property_status(&self, property: &str, status: PropertyStatus) {
        let mut status_map = self.property_status.lock().unwrap();
        status_map.insert(property.to_string(), status);
    }

    pub fn get_all_property_statuses(&self) -> HashMap<String, PropertyStatus> {
        let status = self.property_status.lock().unwrap();
        status.clone()
    }

    pub fn id(&self) -> Uuid {
        let base = self.base.read().unwrap();
        base.metainfo.id
    }
}

impl<TC, T1, T2> PayloadTrait for CogneePayload<TC, T1, T2>
where
    TC: Clone + Send + Sync + 'static,
    T1: Clone + Send + Sync + 'static,
    T2: Clone + Send + Sync + 'static,
{
    fn payload_id(&self) -> Uuid {
        self.id()
    }

    fn payload_get_property_status(&self, property: &str) -> Option<PropertyStatus> {
        self.get_property_status(property)
    }

    fn payload_set_property_status(&self, property: &str, status: PropertyStatus) {
        self.set_property_status(property, status)
    }

    fn payload_get_arc(
        &self,
        property: &str,
    ) -> Result<Box<dyn std::any::Any + Send + Sync>, String> {
        self.get_arc(property)
    }

    fn payload_get_copy(
        &self,
        property: &str,
    ) -> Result<Box<dyn std::any::Any + Send + Sync>, String> {
        self.get_copy(property)
    }

    fn payload_get_all_property_statuses(&self) -> HashMap<String, PropertyStatus> {
        self.get_all_property_statuses()
    }
}

// ============================================================================
// create_cognee_payload! - Macro for generating custom payload types
// ============================================================================

#[macro_export]
macro_rules! create_cognee_payload {
    ($name:ident, $($result_name:ident: $result_type:ty),*) => {
        #[derive(Debug, Clone)]
        pub struct $name
        where
            $(
                $result_type: Clone + Send + Sync + 'static,
            )*
        {
            base: Arc<RwLock<PayloadBase>>,
            chunks: Arc<RwLock<Vec<Arc<String>>>>,
            $(
                $result_name: Arc<RwLock<Vec<Arc<$result_type>>>>,
            )*
            property_status: Arc<Mutex<HashMap<String, PropertyStatus>>>,
        }

        impl $name
        where
            $(
                $result_type: Clone + Send + Sync + 'static,
            )*
        {
            #[allow(dead_code)]
            pub fn new(chunks: Vec<Arc<String>>) -> Self {
                let mut status = HashMap::new();
                status.insert("base".to_string(), PropertyStatus::Done);
                status.insert(
                    "chunks".to_string(),
                    if chunks.is_empty() {
                        PropertyStatus::Empty
                    } else {
                        PropertyStatus::Done
                    },
                );
                $(
                    status.insert(stringify!($result_name).to_string(), PropertyStatus::Empty);
                )*

                Self {
                    base: Arc::new(RwLock::new(PayloadBase::new())),
                    chunks: Arc::new(RwLock::new(chunks)),
                    $(
                        $result_name: Arc::new(RwLock::new(Vec::new())),
                    )*
                    property_status: Arc::new(Mutex::new(status)),
                }
            }

            pub fn get_arc(&self, property: &str) -> Result<Box<dyn std::any::Any + Send + Sync>, String> {
                match property {
                    "base" => Ok(Box::new(Arc::clone(&self.base))),
                    "chunks" => Ok(Box::new(Arc::clone(&self.chunks))),
                    $(
                        stringify!($result_name) => Ok(Box::new(Arc::clone(&self.$result_name))),
                    )*
                    "property_status" => Ok(Box::new(Arc::clone(&self.property_status))),
                    _ => Err(format!("Unknown property: {property}")),
                }
            }

            pub fn get_copy(&self, property: &str) -> Result<Box<dyn std::any::Any + Send + Sync>, String> {
                match property {
                    "chunks" => {
                        let chunks = self.chunks.read().unwrap();
                        Ok(Box::new(chunks.clone()))
                    }
                    $(
                        stringify!($result_name) => {
                            let result = self.$result_name.read().unwrap();
                            Ok(Box::new(result.clone()))
                        }
                    )*
                    _ => Err(format!("Unknown property: {property}")),
                }
            }

            pub fn get_property_status(&self, property: &str) -> Option<PropertyStatus> {
                let status = self.property_status.lock().unwrap();
                status.get(property).cloned()
            }

            pub fn set_property_status(&self, property: &str, status: PropertyStatus) {
                let mut status_map = self.property_status.lock().unwrap();
                status_map.insert(property.to_string(), status);
            }

            pub fn get_all_property_statuses(&self) -> HashMap<String, PropertyStatus> {
                let status = self.property_status.lock().unwrap();
                status.clone()
            }

            pub fn base(&self) -> Arc<RwLock<PayloadBase>> {
                Arc::clone(&self.base)
            }

            pub fn id(&self) -> uuid::Uuid {
                let base = self.base.read().unwrap();
                base.metainfo.id
            }
        }

        impl $crate::data::PayloadTrait for $name
        where
            $(
                $result_type: Clone + Send + Sync + 'static,
            )*
        {
            fn payload_id(&self) -> uuid::Uuid {
                self.id()
            }

            fn payload_get_property_status(&self, property: &str) -> Option<PropertyStatus> {
                self.get_property_status(property)
            }

            fn payload_set_property_status(&self, property: &str, status: PropertyStatus) {
                self.set_property_status(property, status)
            }

            fn payload_get_arc(&self, property: &str) -> Result<Box<dyn std::any::Any + Send + Sync>, String> {
                self.get_arc(property)
            }

            fn payload_get_copy(&self, property: &str) -> Result<Box<dyn std::any::Any + Send + Sync>, String> {
                self.get_copy(property)
            }

            fn payload_get_all_property_statuses(&self) -> HashMap<String, PropertyStatus> {
                self.get_all_property_statuses()
            }
        }

        impl $crate::data::PayloadConstructor for $name
        where
            $(
                $result_type: Clone + Send + Sync + 'static,
            )*
        {
            fn new(chunks: Vec<Arc<String>>) -> Self {
                Self::new(chunks)
            }
        }
    };
}
