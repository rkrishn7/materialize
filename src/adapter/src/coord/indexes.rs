// Copyright Materialize, Inc. and contributors. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use std::collections::BTreeSet;

use mz_compute_client::controller::ComputeInstanceId;
use mz_expr::{CollectionPlan, MirScalarExpr};
use mz_repr::GlobalId;
use mz_transform::IndexOracle;

use crate::catalog::{CatalogItem, Index, Log};
use crate::coord::dataflows::DataflowBuilder;
use crate::coord::{CollectionIdBundle, Coordinator};

impl Coordinator {
    /// Creates a new index oracle for the specified compute instance.
    pub fn index_oracle(&self, instance: ComputeInstanceId) -> DataflowBuilder {
        self.dataflow_builder(instance)
    }
}

impl DataflowBuilder<'_> {
    /// Identifies a bundle of storage and compute collection ids sufficient for
    /// building a dataflow for the identifiers in `ids` out of the indexes
    /// available in this compute instance.
    pub fn sufficient_collections<'a, I>(&self, ids: I) -> CollectionIdBundle
    where
        I: IntoIterator<Item = &'a GlobalId>,
    {
        let mut id_bundle = CollectionIdBundle::default();
        let mut todo: BTreeSet<GlobalId> = ids.into_iter().cloned().collect();

        // Iteratively extract the largest element, potentially introducing lesser elements.
        while let Some(id) = todo.iter().rev().next().cloned() {
            // Extract available indexes as those that are enabled, and installed on the cluster.
            let mut available_indexes = self.indexes_on(id).map(|(id, _)| id).peekable();

            if available_indexes.peek().is_some() {
                id_bundle
                    .compute_ids
                    .entry(self.compute.instance_id())
                    .or_default()
                    .extend(available_indexes);
            } else {
                match self.catalog.get_entry(&id).item() {
                    // Unmaterialized view. Search its dependencies.
                    CatalogItem::View(view) => {
                        todo.extend(view.optimized_expr.0.depends_on());
                    }
                    CatalogItem::Source(_)
                    | CatalogItem::Table(_)
                    | CatalogItem::MaterializedView(_)
                    | CatalogItem::Log(Log {
                        has_storage_collection: true,
                        ..
                    }) => {
                        // Record that we are missing at least one index.
                        id_bundle.storage_ids.insert(id);
                    }
                    CatalogItem::Log(Log {
                        has_storage_collection: false,
                        ..
                    }) => {
                        // Log sources without storage collections should always
                        // be protected by an index.
                        panic!("log source without storage collection {id} is missing index");
                    }
                    _ => {
                        // Non-indexable thing; no work to do.
                    }
                }
            }
            todo.remove(&id);
        }

        id_bundle
    }

    pub fn indexes_on(&self, id: GlobalId) -> impl Iterator<Item = (GlobalId, &Index)> {
        self.catalog
            .get_indexes_on(id, self.compute.instance_id())
            .filter(|(idx_id, _idx)| self.compute.contains_collection(idx_id))
    }
}

impl IndexOracle for DataflowBuilder<'_> {
    fn indexes_on(&self, id: GlobalId) -> Box<dyn Iterator<Item = &[MirScalarExpr]> + '_> {
        Box::new(
            self.indexes_on(id)
                .map(|(_idx_id, idx)| idx.keys.as_slice()),
        )
    }
}
