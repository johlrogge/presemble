# Async Tokio Patterns and Best Practices

## Tokio Runtime Setup

### Standard Application
```rust
#[tokio::main]
async fn main() {
    // Multi-threaded runtime by default
}

// Equivalent to:
fn main() {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            // ...
        })
}
```

### Custom Runtime
```rust
use tokio::runtime::Runtime;

fn main() {
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        run_app().await;
    });
}
```

### Current-thread Runtime (for tests)
```rust
#[tokio::test(flavor = "current_thread")]
async fn test_something() {
    // Single-threaded, deterministic
}
```

## Common Patterns

### Pattern: Spawning Tasks

```rust
use tokio::task;

async fn process_items(items: Vec<Item>) {
    let mut handles = vec![];

    for item in items {
        let handle = task::spawn(async move {
            process_item(item).await
        });
        handles.push(handle);
    }

    // Wait for all tasks
    for handle in handles {
        handle.await.unwrap();
    }
}
```

### Pattern: Join Multiple Futures

```rust
use tokio::join;

async fn fetch_all() -> (User, Posts, Comments) {
    // Run concurrently, wait for all
    let (user, posts, comments) = join!(
        fetch_user(),
        fetch_posts(),
        fetch_comments(),
    );

    (user, posts, comments)
}
```

### Pattern: Select First Completed

```rust
use tokio::select;

async fn race() {
    select! {
        result = fetch_from_primary() => {
            println!("Primary: {:?}", result);
        }
        result = fetch_from_backup() => {
            println!("Backup: {:?}", result);
        }
    }
}
```

### Pattern: Timeout

```rust
use tokio::time::{timeout, Duration};

async fn with_timeout() -> Result<Data, Error> {
    match timeout(Duration::from_secs(5), fetch_data()).await {
        Ok(Ok(data)) => Ok(data),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(Error::Timeout),
    }
}
```

### Pattern: Interval/Periodic Tasks

```rust
use tokio::time::{interval, Duration};

async fn periodic_cleanup() {
    let mut interval = interval(Duration::from_secs(60));

    loop {
        interval.tick().await;
        cleanup().await;
    }
}
```

## Channels

### mpsc: Multiple Producer, Single Consumer

```rust
use tokio::sync::mpsc;

async fn producer_consumer() {
    let (tx, mut rx) = mpsc::channel(100);

    // Spawn producers
    for i in 0..10 {
        let tx = tx.clone();
        tokio::spawn(async move {
            tx.send(i).await.unwrap();
        });
    }
    drop(tx); // Close channel when done sending

    // Consumer
    while let Some(msg) = rx.recv().await {
        println!("Received: {}", msg);
    }
}
```

### oneshot: Single Message

```rust
use tokio::sync::oneshot;

async fn request_response() {
    let (tx, rx) = oneshot::channel();

    tokio::spawn(async move {
        let result = compute().await;
        tx.send(result).ok();
    });

    match rx.await {
        Ok(result) => println!("Got: {}", result),
        Err(_) => println!("Sender dropped"),
    }
}
```

### broadcast: Multiple Consumers

```rust
use tokio::sync::broadcast;

async fn pub_sub() {
    let (tx, mut rx1) = broadcast::channel(16);
    let mut rx2 = tx.subscribe();

    tokio::spawn(async move {
        tx.send("message").unwrap();
    });

    let msg1 = rx1.recv().await.unwrap();
    let msg2 = rx2.recv().await.unwrap();
    assert_eq!(msg1, msg2);
}
```

### watch: State Broadcasting

```rust
use tokio::sync::watch;

async fn state_updates() {
    let (tx, mut rx) = watch::channel("initial");

    tokio::spawn(async move {
        tx.send("updated").unwrap();
    });

    rx.changed().await.unwrap();
    println!("New value: {}", *rx.borrow());
}
```

## Synchronization Primitives

### Mutex

```rust
use tokio::sync::Mutex;
use std::sync::Arc;

async fn shared_state() {
    let data = Arc::new(Mutex::new(vec![]));

    let data_clone = data.clone();
    tokio::spawn(async move {
        let mut d = data_clone.lock().await;
        d.push(1);
    });

    let d = data.lock().await;
    println!("{:?}", *d);
}
```

**When to use:**
- tokio::sync::Mutex for async code (lock held across .await)
- std::sync::Mutex for sync code (no .await while locked)

### RwLock

```rust
use tokio::sync::RwLock;

async fn read_heavy() {
    let data = Arc::new(RwLock::new(vec![1, 2, 3]));

    // Multiple concurrent readers
    let read = data.read().await;
    println!("{:?}", *read);

    // Exclusive writer
    let mut write = data.write().await;
    write.push(4);
}
```

### Semaphore (Rate Limiting)

```rust
use tokio::sync::Semaphore;

async fn rate_limit() {
    let semaphore = Arc::new(Semaphore::new(10)); // Max 10 concurrent

    for i in 0..100 {
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        tokio::spawn(async move {
            process(i).await;
            drop(permit); // Release
        });
    }
}
```

## Common Pitfalls

### Pitfall: Blocking in Async

```rust
// ❌ Blocks the executor thread
async fn bad() {
    std::thread::sleep(Duration::from_secs(1));
}

// ✅ Use async sleep
async fn good() {
    tokio::time::sleep(Duration::from_secs(1)).await;
}

// ✅ Or spawn_blocking for CPU-heavy work
async fn cpu_intensive() {
    let result = tokio::task::spawn_blocking(|| {
        // Expensive computation
        compute_heavy()
    }).await.unwrap();
}
```

### Pitfall: Send Bounds

```rust
// ❌ Rc is not Send
async fn bad() {
    let data = Rc::new(vec![1, 2, 3]);
    tokio::spawn(async move {
        // Error: Rc is not Send
        println!("{:?}", data);
    });
}

// ✅ Use Arc
async fn good() {
    let data = Arc::new(vec![1, 2, 3]);
    tokio::spawn(async move {
        println!("{:?}", data);
    });
}
```

### Pitfall: Holding Locks Across Await

```rust
use std::sync::Mutex;

// ❌ Can cause deadlock
async fn bad(data: Arc<Mutex<Vec<i32>>>) {
    let mut d = data.lock().unwrap();
    some_async_operation().await; // Lock held across await!
    d.push(1);
}

// ✅ Drop lock before await
async fn good(data: Arc<Mutex<Vec<i32>>>) {
    {
        let mut d = data.lock().unwrap();
        d.push(1);
    } // Lock dropped here
    some_async_operation().await;
}

// ✅ Or use tokio::sync::Mutex
async fn good2(data: Arc<tokio::sync::Mutex<Vec<i32>>>) {
    let mut d = data.lock().await;
    some_async_operation().await; // OK with tokio::sync::Mutex
    d.push(1);
}
```

### Pitfall: Unbounded Spawning

```rust
// ❌ Can overwhelm system
async fn bad(items: Vec<Item>) {
    for item in items {
        tokio::spawn(async move {
            process(item).await;
        });
    }
}

// ✅ Use Semaphore for backpressure
async fn good(items: Vec<Item>) {
    let semaphore = Arc::new(Semaphore::new(10));

    for item in items {
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        tokio::spawn(async move {
            process(item).await;
            drop(permit);
        });
    }
}

// ✅ Or use a buffered stream
use futures::stream::{self, StreamExt};

async fn good2(items: Vec<Item>) {
    stream::iter(items)
        .for_each_concurrent(10, |item| async move {
            process(item).await;
        })
        .await;
}
```

## Structured Concurrency with JoinSet

```rust
use tokio::task::JoinSet;

async fn structured() -> Result<Vec<String>> {
    let mut set = JoinSet::new();

    for i in 0..10 {
        set.spawn(async move {
            fetch_item(i).await
        });
    }

    let mut results = vec![];
    while let Some(res) = set.join_next().await {
        results.push(res??);
    }

    Ok(results)
}
```

## Graceful Shutdown

```rust
use tokio::signal;

async fn shutdown_signal() {
    signal::ctrl_c()
        .await
        .expect("failed to listen for ctrl-c");
}

#[tokio::main]
async fn main() {
    let handle = tokio::spawn(async {
        // Long-running task
    });

    shutdown_signal().await;
    println!("Shutting down...");

    handle.abort();
    handle.await.ok();
}
```

## Testing

```rust
#[tokio::test]
async fn test_async_function() {
    let result = my_async_function().await;
    assert_eq!(result, expected);
}

// Time manipulation in tests
#[tokio::test]
async fn test_with_time() {
    tokio::time::pause();

    let start = Instant::now();
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Time didn't actually pass
    assert!(start.elapsed() < Duration::from_millis(100));
}
```

## Performance Tips

1. **Use spawn_blocking for CPU work** - Don't block async threads
2. **Batch small operations** - Overhead per task/future adds up
3. **Choose appropriate channel bounds** - Balance memory vs backpressure
4. **Profile with tokio-console** - Visualize task execution
5. **Use `#[inline]` for hot paths** - Small async functions benefit
6. **Consider multi_thread vs current_thread** - Apps need multi, tests may want current
