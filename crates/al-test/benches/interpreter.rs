//! Interpreter performance benchmarks for al-core.
//!
//! Run with:
//!   cargo bench -p al-core
//!
//! Each benchmark sets up all state **outside** the measurement loop so that
//! only the hot path is timed.

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};

use al_runtime::interpreter::dispatch::{dispatch_call, DispatchCtx};
use al_runtime::interpreter::value::{Decimal, Value};
use al_runtime::mock::record::MockRecord;
use al_runtime::stubs;

// Tight loop of integer and decimal arithmetic via the dispatch layer's
// inline builtins: repeated addition, subtraction, and modulo via
// Value arithmetic.  The 1000-iteration accumulation is done inside
// a single `b.iter` call so Criterion measures the amortised per-loop cost.
fn bench_arithmetic(c: &mut Criterion) {
    // Pre-allocate values that will be reused on every iteration.
    let base_int = Value::Integer(0);
    let step_int = Value::Integer(7);
    let base_dec = Value::Decimal(Decimal::ZERO);
    let step_dec = Value::Decimal(Decimal::new(35, 1));

    c.bench_function("arithmetic/integer_accumulate_1000", |b| {
        b.iter(|| {
            let mut acc = base_int.clone();
            for _ in 0..1000 {
                // Simulate integer addition: call the Format builtin so we
                // exercise the dispatch table + value rendering hot-path.
                let result = dispatch_call(
                    None,
                    "format",
                    vec![black_box(acc.clone())],
                    // Pure-logic ctx without a workspace — Format never needs one.
                    &mut DispatchCtx::new_pure(al_workspace::Workspace::new().file_index),
                );
                if let Value::Integer(n) = &acc {
                    acc = Value::Integer(
                        n + if let Value::Integer(s) = &step_int {
                            *s
                        } else {
                            1
                        },
                    );
                }
                black_box(result);
            }
            black_box(acc)
        });
    });

    c.bench_function("arithmetic/decimal_accumulate_1000", |b| {
        b.iter(|| {
            let mut acc = base_dec.clone();
            for _ in 0..1000 {
                if let Value::Decimal(n) = &acc {
                    if let Value::Decimal(s) = &step_dec {
                        acc = Value::Decimal(*n + *s);
                    }
                }
            }
            black_box(acc)
        });
    });
}

// Exercises StrSubstNo, CopyStr, IndexOf, and Format on three payload sizes.
fn bench_string_ops(c: &mut Criterion) {
    let small = "Hello, World!".to_string();
    let medium = "The quick brown fox jumps over the lazy dog. ".repeat(10);
    let large = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(100);

    let mut group = c.benchmark_group("string_ops");

    for (label, s) in [("small", &small), ("medium", &medium), ("large", &large)] {
        let fmt_str = "Result: %1, Again: %2, Third: %3".to_string();
        let arg1 = Value::Text(s.clone());
        let arg2 = Value::Integer(42);
        let arg3 = Value::Boolean(true);

        group.bench_function(format!("StrSubstNo/{label}"), |b| {
            b.iter_batched(
                || {
                    (
                        Value::Text(fmt_str.clone()),
                        arg1.clone(),
                        arg2.clone(),
                        arg3.clone(),
                    )
                },
                |(fmt, a1, a2, a3)| {
                    let mut ctx = DispatchCtx::new_pure(al_workspace::Workspace::new().file_index);
                    black_box(dispatch_call(
                        None,
                        "StrSubstNo",
                        vec![fmt, a1, a2, a3],
                        &mut ctx,
                    ))
                },
                BatchSize::SmallInput,
            );
        });
    }

    for (label, s) in [("small", &small), ("medium", &medium), ("large", &large)] {
        let len = s.chars().count();
        let mid = (len / 2).max(1);
        let extract_len = (len / 4).max(1);

        group.bench_function(format!("CopyStr/{label}"), |b| {
            b.iter_batched(
                || Value::Text(s.clone()),
                |val| {
                    let mut ctx = DispatchCtx::new_pure(al_workspace::Workspace::new().file_index);
                    black_box(dispatch_call(
                        None,
                        "CopyStr",
                        vec![
                            val,
                            Value::Integer(mid as i64),
                            Value::Integer(extract_len as i64),
                        ],
                        &mut ctx,
                    ))
                },
                BatchSize::SmallInput,
            );
        });
    }

    for (label, s) in [("small", &small), ("medium", &medium), ("large", &large)] {
        let needle = "o".to_string();

        group.bench_function(format!("IndexOf/{label}"), |b| {
            b.iter_batched(
                || (Value::Text(s.clone()), Value::Text(needle.clone())),
                |(haystack, n)| {
                    let mut ctx = DispatchCtx::new_pure(al_workspace::Workspace::new().file_index);
                    black_box(dispatch_call(None, "IndexOf", vec![haystack, n], &mut ctx))
                },
                BatchSize::SmallInput,
            );
        });
    }

    for (label, s) in [("small", &small), ("medium", &medium), ("large", &large)] {
        group.bench_function(format!("Format/{label}"), |b| {
            b.iter_batched(
                || Value::Text(s.clone()),
                |val| {
                    let mut ctx = DispatchCtx::new_pure(al_workspace::Workspace::new().file_index);
                    black_box(dispatch_call(None, "Format", vec![val], &mut ctx))
                },
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

// Insert/Get/Modify/Delete cycle on a MockRecord, 100 records per iteration.
// The record is pre-created outside the measurement loop; only the CRUD
// cycle itself is timed.
fn bench_record_crud(c: &mut Criterion) {
    c.bench_function("record_crud/insert_find_modify_delete_100", |b| {
        b.iter_batched(
            || {
                // Setup: empty table with PK on field 1.
                MockRecord::new(27, "Item", vec![1])
            },
            |mut rec| {
                for i in 1i64..=100 {
                    rec.field_set(1, Value::Integer(i));
                    rec.field_set(2, Value::Text(format!("Item {i}")));
                    rec.insert(false).expect("insert");
                }

                for i in 1i64..=100 {
                    rec.get(vec![Value::Integer(i)]).expect("get");
                    rec.field_set(2, Value::Text(format!("Modified {i}")));
                    rec.modify(false).expect("modify");
                }

                for i in (1i64..=100).step_by(2) {
                    rec.field_set(1, Value::Integer(i));
                    rec.delete(false).expect("delete");
                }

                black_box(rec.count())
            },
            BatchSize::SmallInput,
        );
    });
}

// Apply a compound filter (range + OR pattern + wildcard) over 1000 records.
// The table is pre-populated outside the loop; the filter + FindSet + Next
// walk is the hot path.
fn bench_filter_apply(c: &mut Criterion) {
    // Build the 1000-row table once and reuse it in every iteration.
    let table = {
        let mut rec = MockRecord::new(18, "Customer", vec![1]);
        for i in 1i64..=1000 {
            rec.field_set(1, Value::Integer(i));
            rec.field_set(2, Value::Text(format!("Customer {i:04}")));
            rec.field_set(3, Value::Integer(i % 10)); // Group 0-9
            rec.insert(false).expect("insert");
        }
        rec
    };

    c.bench_function("filter_apply/range_1000_records", |b| {
        b.iter_batched(
            || table.clone(),
            |mut rec| {
                rec.set_range(1, Value::Integer(100), Value::Integer(500));

                let found = rec.find_set().expect("find_set");
                let mut count = 0usize;
                if found {
                    count += 1;
                    while rec.next(1).expect("next") > 0 {
                        count += 1;
                    }
                }
                black_box(count)
            },
            BatchSize::SmallInput,
        );
    });

    c.bench_function("filter_apply/filter_expr_wildcard_1000_records", |b| {
        b.iter_batched(
            || table.clone(),
            |mut rec| {
                rec.set_filter(3, ">0&<5").expect("set_filter");

                let found = rec.find_set().expect("find_set");
                let mut count = 0usize;
                if found {
                    count += 1;
                    while rec.next(1).expect("next") > 0 {
                        count += 1;
                    }
                }
                black_box(count)
            },
            BatchSize::SmallInput,
        );
    });
}

// Hot-path cost of resolving and executing Library Assert stubs:
// AreEqual and IsTrue on already-evaluated Value arguments.
fn bench_library_assert_call(c: &mut Criterion) {
    let mut group = c.benchmark_group("library_assert");

    group.bench_function("AreEqual/integer_match", |b| {
        b.iter(|| {
            black_box(stubs::resolve("Library Assert", "AreEqual").unwrap()(&[
                black_box(Value::Integer(42)),
                black_box(Value::Integer(42)),
            ]))
        });
    });

    group.bench_function("AreEqual/integer_mismatch_error_path", |b| {
        b.iter(|| {
            black_box(stubs::resolve("Library Assert", "AreEqual").unwrap()(&[
                black_box(Value::Integer(1)),
                black_box(Value::Integer(2)),
            ]))
        });
    });

    group.bench_function("IsTrue/pass", |b| {
        b.iter(|| {
            black_box(stubs::resolve("Library Assert", "IsTrue").unwrap()(&[
                black_box(Value::Boolean(true)),
                black_box(Value::Text("should not fail".into())),
            ]))
        });
    });

    group.bench_function("IsTrue/fail_error_path", |b| {
        b.iter(|| {
            black_box(stubs::resolve("Library Assert", "IsTrue").unwrap()(&[
                black_box(Value::Boolean(false)),
                black_box(Value::Text("expected true".into())),
            ]))
        });
    });

    group.finish();
}

// Router classification of a moderately-complex codeunit (~20 procedures,
// various signal patterns).  The workspace is built once; `classify_all`
// is the hot path.
fn bench_callgraph_walk(c: &mut Criterion) {
    use al_test::router::classify_all;
    use al_workspace::Workspace;
    use std::path::PathBuf;
    use std::sync::Arc;

    // Medium codeunit: mix of pure-logic tests, record-touching tests, and
    // HTTP-escape tests so the router exercises all three PATTERNS buckets.
    let codeunit_src = r#"
codeunit 50100 "BenchmarkTests"
{
    Subtype = Test;

    [Test]
    procedure PureArithmetic()
    var
        x: Integer;
        y: Decimal;
    begin
        x := 10 + 20;
        y := 3.14 * x;
        Assert.AreEqual(300, x * 10, 'multiplication');
        Assert.IsTrue(y > 0.0, 'positive');
    end;

    [Test]
    procedure StringManipulation()
    var
        s: Text[250];
        result: Text[250];
    begin
        s := 'Hello, World!';
        result := CopyStr(s, 1, 5);
        Assert.AreEqual('Hello', result, 'CopyStr');
        Assert.AreEqual(13, StrLen(s), 'StrLen');
    end;

    [Test]
    procedure FormatNumbers()
    var
        i: Integer;
        t: Text[50];
    begin
        for i := 1 to 100 do begin
            t := Format(i);
            Assert.IsTrue(StrLen(t) > 0, 'non-empty');
        end;
    end;

    [Test]
    procedure RecordInsertAndGet()
    var
        Cust: Record Customer;
    begin
        Cust.Init();
        Cust."No." := 'C001';
        Cust.Insert(false);
        Cust.Get('C001');
        Assert.AreEqual('C001', Cust."No.", 'round-trip');
    end;

    [Test]
    procedure RecordFindSet()
    var
        Cust: Record Customer;
    begin
        Cust.SetRange("Country/Region Code", 'GB');
        if Cust.FindSet() then
            repeat
                Assert.IsTrue(Cust."No." <> '', 'No. non-empty');
            until Cust.Next() = 0;
    end;

    [Test]
    procedure HttpClientTest()
    var
        Client: HttpClient;
        Req: HttpRequestMessage;
        Resp: HttpResponseMessage;
    begin
        Req.SetRequestUri('https://example.com/api');
        Client.Send(Req, Resp);
        Assert.IsTrue(Resp.IsSuccessStatusCode(), 'status ok');
    end;

    [Test]
    procedure ModifyAndDelete()
    var
        Item: Record Item;
    begin
        Item.Get('ITEM-01');
        Item.Description := 'Updated';
        Item.Modify(false);
        Item.Delete(false);
    end;

    [Test]
    procedure CaseAndLoop()
    var
        i: Integer;
        s: Text[20];
    begin
        for i := 1 to 10 do begin
            case i mod 3 of
                0: s := 'fizz';
                1: s := 'one';
                2: s := 'two';
            end;
        end;
        Assert.AreEqual('two', s, 'last case');
    end;

    [Test]
    procedure WhileLoop()
    var
        n: Integer;
    begin
        n := 1;
        while n < 1000 do
            n := n * 2;
        Assert.IsTrue(n >= 1000, 'pow2 loop');
    end;

    [Test]
    procedure NestedProcCall()
    begin
        Helper1();
        Helper2();
    end;

    local procedure Helper1()
    var
        x: Integer;
    begin
        x := 1 + 1;
    end;

    local procedure Helper2()
    var
        y: Text[10];
    begin
        y := StrSubstNo('Value: %1', 42);
    end;
}
"#;

    let ws = Arc::new(Workspace::new());
    let path = PathBuf::from("/bench/BenchmarkTests.al");
    ws.file_index.add_file(path, codeunit_src.to_string());

    c.bench_function("callgraph_walk/classify_all_medium_codeunit", |b| {
        b.iter(|| black_box(classify_all(&ws)));
    });
}

criterion_group!(
    benches,
    bench_arithmetic,
    bench_string_ops,
    bench_record_crud,
    bench_filter_apply,
    bench_library_assert_call,
    bench_callgraph_walk,
);
criterion_main!(benches);
