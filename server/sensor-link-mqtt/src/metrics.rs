use opentelemetry::{global, KeyValue};
use std::fmt::Display;

/// Increments a counter metric with the given name and attributes
///
/// # Arguments
/// * `meter_name` - The name of the meter to use
/// * `counter_name` - The name of the counter to increment
/// * `value` - The value to add to the counter
/// * `attributes` - A slice of tuples containing attribute name and value
pub fn increment_counter<T, V>(
    meter_name: &'static str,
    counter_name: &'static str,
    value: T,
    attributes: &[(&'static str, V)],
) where
    T: Into<u64>,
    V: Display,
{
    let meter = global::meter(meter_name);

    let counter = meter.u64_counter(counter_name).build();
    let key_values: Vec<KeyValue> = attributes
        .iter()
        .map(|(k, v)| KeyValue::new(*k, v.to_string()))
        .collect();
    counter.add(value.into(), key_values.as_slice());
}

/// Records a value in a histogram metric with the given name and attributes
///
/// # Arguments
/// * `meter_name` - The name of the meter to use
/// * `histogram_name` - The name of the histogram to record in
/// * `value` - The value to record
/// * `attributes` - A slice of tuples containing attribute name and value
pub fn record_histogram<V>(
    meter_name: &'static str,
    histogram_name: &'static str,
    value: f64,
    attributes: &[(&'static str, V)],
) where
    V: Display,
{
    let meter = global::meter(meter_name);

    let histogram = meter.f64_histogram(histogram_name).build();
    let key_values: Vec<KeyValue> = attributes
        .iter()
        .map(|(k, v)| KeyValue::new(*k, v.to_string()))
        .collect();
    histogram.record(value, key_values.as_slice());
}

/// Records the current value of a gauge metric with the given name and attributes
///
/// # Arguments
/// * `meter_name` - The name of the meter to use
/// * `gauge_name` - The name of the gauge to record
/// * `value` - The current value
/// * `attributes` - A slice of tuples containing attribute name and value
pub fn record_gauge<T, V>(
    meter_name: &'static str,
    gauge_name: &'static str,
    value: T,
    attributes: &[(&'static str, V)],
) where
    T: Into<u64>,
    V: Display,
{
    let meter = global::meter(meter_name);

    let gauge = meter.u64_gauge(gauge_name).build();
    let key_values: Vec<KeyValue> = attributes
        .iter()
        .map(|(k, v)| KeyValue::new(*k, v.to_string()))
        .collect();
    gauge.record(value.into(), key_values.as_slice());
}

/// Increments a counter metric with the given name and a single attribute
pub fn increment_counter_with_attribute<T, V>(
    meter_name: &'static str,
    counter_name: &'static str,
    value: T,
    attr_name: &'static str,
    attr_value: V,
) where
    T: Into<u64>,
    V: Display,
{
    increment_counter(meter_name, counter_name, value, &[(attr_name, attr_value)]);
}
