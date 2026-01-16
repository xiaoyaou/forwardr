use criterion::{BatchSize, BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use const_string::ConstString;

const SHORT_ASCII: &str = "0123456789ABCDE"; // <= 15 bytes -> stack variant
const LONG_ASCII: &str = "0123456789ABCDEF"; // > 15 bytes -> heap variant

fn bench_methods_for(c: &mut Criterion, id: &str, sample: &str) {
    let mut group = c.benchmark_group(format!("ConstString::{}", id));

    // Prepare instances used for non-consuming operations.
    let mut cs = ConstString::new(sample);

    // len()
    group.bench_function(BenchmarkId::new("len", id), |b| {
        b.iter(|| {
            black_box(cs.len());
        });
    });

    // as_str()
    group.bench_function(BenchmarkId::new("as_str", id), |b| {
        b.iter(|| {
            black_box(cs.as_str());
        });
    });

    // as_mut_str()
    group.bench_function(BenchmarkId::new("as_mut_str", id), |b| {
        b.iter(|| {
            black_box(cs.as_mut_str());
        });
    });

    // into_string() consumes the ConstString; use iter_batched to create a fresh input each iteration.
    group.bench_function(BenchmarkId::new("into_string", id), |b| {
        b.iter_batched(
            || cs.clone(),
            |cs| {
                let s = cs.into_string();
                black_box(s);
            },
            BatchSize::SmallInput,
        )
    });

    group.bench_function(BenchmarkId::new("drop", id), |b| {
        b.iter_batched(
            || cs.clone(),
            |cs| {
                black_box(cs);
            },
            BatchSize::SmallInput,
        )
    });

    group.finish();
}

fn criterion_benchmarks(c: &mut Criterion) {
    bench_methods_for(c, "short", SHORT_ASCII);
    bench_methods_for(c, "long", LONG_ASCII);
}

criterion_group!(benches, criterion_benchmarks);
criterion_main!(benches);
