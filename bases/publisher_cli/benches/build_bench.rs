mod site_gen;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::time::Duration;

fn bench_full_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_build");
    group.sample_size(10); // fewer samples for expensive benchmarks
    group.measurement_time(Duration::from_secs(30));

    for &pages in &[10, 100, 1000, 10_000] {
        if pages == 10_000 {
            group.warm_up_time(Duration::from_secs(5));
            group.sample_size(10);
        }
        let config = site_gen::BenchSiteConfig {
            pages,
            schemas: 3,
            links_per_page: 3,
            link_expressions: 0,
            body_bytes: 2000,
        };
        let dir = site_gen::generate_site(&config);

        group.bench_with_input(
            BenchmarkId::new("pages", pages),
            &dir,
            |b, dir| {
                b.iter(|| {
                    publisher_cli::build_for_serve(dir, &publisher_cli::UrlConfig::default())
                        .expect("build should succeed")
                })
            },
        );
    }
    group.finish();
}

fn bench_cross_links(c: &mut Criterion) {
    let mut group = c.benchmark_group("cross_links");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));

    for &links in &[0, 1, 3, 5, 10] {
        let config = site_gen::BenchSiteConfig {
            pages: 500,
            schemas: 3,
            links_per_page: links,
            link_expressions: 0,
            body_bytes: 2000,
        };
        let dir = site_gen::generate_site(&config);

        group.bench_with_input(
            BenchmarkId::new("links", links),
            &dir,
            |b, dir| {
                b.iter(|| {
                    publisher_cli::build_for_serve(dir, &publisher_cli::UrlConfig::default())
                        .expect("build should succeed")
                })
            },
        );
    }
    group.finish();
}

fn bench_link_expressions(c: &mut Criterion) {
    let mut group = c.benchmark_group("link_expressions");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));

    for &exprs in &[0, 1, 3, 5] {
        let config = site_gen::BenchSiteConfig {
            pages: 500,
            schemas: 3,
            links_per_page: 1,
            link_expressions: exprs,
            body_bytes: 2000,
        };
        let dir = site_gen::generate_site(&config);

        group.bench_with_input(
            BenchmarkId::new("expressions", exprs),
            &dir,
            |b, dir| {
                b.iter(|| {
                    publisher_cli::build_for_serve(dir, &publisher_cli::UrlConfig::default())
                        .expect("build should succeed")
                })
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_full_build, bench_cross_links, bench_link_expressions);
criterion_main!(benches);
