// Copyright Materialize, Inc. and contributors. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use mz_compute_client::metrics::{CommandMetrics, HistoryMetrics};
use mz_ore::metric;
use mz_ore::metrics::{raw, DeleteOnDropGauge, GaugeVec, GaugeVecExt, MetricsRegistry, UIntGauge};
use mz_repr::GlobalId;
use prometheus::core::{AtomicF64, GenericCounter};

/// Metrics exposed by compute replicas.
#[derive(Clone, Debug)]
pub struct ComputeMetrics {
    // command history
    history_command_count: raw::UIntGaugeVec,
    history_dataflow_count: UIntGauge,

    // reconciliation
    reconciliation_reused_dataflows_count_total: raw::IntCounterVec,
    reconciliation_replaced_dataflows_count_total: raw::IntCounterVec,

    // dataflow state
    dataflow_initial_output_duration_seconds: GaugeVec,

    // arrangements
    arrangement_maintenance_seconds_total: raw::CounterVec,
    arrangement_maintenance_active_info: raw::UIntGaugeVec,
}

impl ComputeMetrics {
    pub fn register_with(registry: &MetricsRegistry) -> Self {
        Self {
            history_command_count: registry.register(metric!(
                name: "mz_compute_replica_history_command_count",
                help: "The number of commands in the replica's command history.",
                var_labels: ["command_type"],
            )),
            history_dataflow_count: registry.register(metric!(
                name: "mz_compute_replica_history_dataflow_count",
                help: "The number of dataflows in the replica's command history.",
            )),
            reconciliation_reused_dataflows_count_total: registry.register(metric!(
                name: "mz_compute_reconciliation_reused_dataflows_count_total",
                help: "The total number of dataflows that were reused during compute reconciliation.",
                var_labels: ["worker_id"],
            )),
            reconciliation_replaced_dataflows_count_total: registry.register(metric!(
                name: "mz_compute_reconciliation_replaced_dataflows_count_total",
                help: "The total number of dataflows that were replaced during compute reconciliation.",
                var_labels: ["worker_id", "reason"],
            )),
            dataflow_initial_output_duration_seconds: registry.register(metric!(
                name: "mz_dataflow_initial_output_duration_seconds",
                help: "The time from dataflow installation up to when the first output was produced.",
                var_labels: ["worker_id", "collection_id"],
            )),
            arrangement_maintenance_seconds_total: registry.register(metric!(
                name: "mz_arrangement_maintenance_seconds_total",
                help: "The total time spent maintaining arrangements.",
                var_labels: ["worker_id"],
            )),
            arrangement_maintenance_active_info: registry.register(metric!(
                name: "mz_arrangement_maintenance_active_info",
                help: "Whether maintenance is currently occuring.",
                var_labels: ["worker_id"],
            )),
        }
    }

    pub fn for_history(&self) -> HistoryMetrics<UIntGauge> {
        let command_counts = CommandMetrics::build(|typ| {
            self.history_command_count
                .get_metric_with_label_values(&[typ])
                .unwrap()
        });
        let dataflow_count = self.history_dataflow_count.clone();

        HistoryMetrics {
            command_counts,
            dataflow_count,
        }
    }

    pub fn for_traces(&self, worker_id: usize) -> TraceMetrics {
        let worker = worker_id.to_string();
        let maintenance_seconds_total = self
            .arrangement_maintenance_seconds_total
            .with_label_values(&[&worker]);
        let maintenance_active_info = self
            .arrangement_maintenance_active_info
            .with_label_values(&[&worker]);

        TraceMetrics {
            maintenance_seconds_total,
            maintenance_active_info,
        }
    }

    pub fn for_collection(
        &self,
        collection_id: GlobalId,
        worker_id: usize,
    ) -> Option<CollectionMetrics> {
        // In an effort to reduce the cardinality of timeseries created, we collect metrics only
        // for non-transient dataflows. This is roughly equivalent to "long-lived" dataflows,
        // with the exception of subscribes which may or may not be long-lived. We might want to
        // change this policy in the future to track subscribes as well.
        if collection_id.is_transient() {
            return None;
        }

        let labels = vec![worker_id.to_string(), collection_id.to_string()];
        let initial_output_duration_seconds = self
            .dataflow_initial_output_duration_seconds
            .get_delete_on_drop_gauge(labels);

        Some(CollectionMetrics {
            initial_output_duration_seconds,
        })
    }

    /// Record the reconciliation result for a single dataflow.
    ///
    /// Reconciliation is recorded as successful if the given properties all hold. Otherwise it is
    /// recorded as unsuccessful, with a reason based on the first property that does not hold.
    pub fn record_dataflow_reconciliation(
        &self,
        worker_id: usize,
        compatible: bool,
        uncompacted: bool,
        subscribe_free: bool,
    ) {
        let worker = worker_id.to_string();

        if !compatible {
            self.reconciliation_replaced_dataflows_count_total
                .with_label_values(&[&worker, "incompatible"])
                .inc();
        } else if !uncompacted {
            self.reconciliation_replaced_dataflows_count_total
                .with_label_values(&[&worker, "compacted"])
                .inc();
        } else if !subscribe_free {
            self.reconciliation_replaced_dataflows_count_total
                .with_label_values(&[&worker, "subscribe"])
                .inc();
        } else {
            self.reconciliation_reused_dataflows_count_total
                .with_label_values(&[&worker])
                .inc();
        }
    }
}

/// Metrics maintained per compute collection.
pub struct CollectionMetrics {
    pub initial_output_duration_seconds: DeleteOnDropGauge<'static, AtomicF64, Vec<String>>,
}

/// Metrics maintained by the trace manager.
pub struct TraceMetrics {
    pub maintenance_seconds_total: GenericCounter<AtomicF64>,
    /// 1 if this worker is currently doing maintenance.
    ///
    /// If maintenance turns out to take a very long time, this will allow us
    /// to gain a sense that Materialize is stuck on maintenance before the
    /// maintenance completes
    pub maintenance_active_info: UIntGauge,
}
