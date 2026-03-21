# Entity Component Systems Beyond Games

## Why ECS Outside Games?

ECS excels at:
- **Dynamic composition** - Add/remove capabilities at runtime
- **Data-oriented design** - Cache-friendly iteration
- **Decoupling** - Systems don't know about each other
- **Massive scale** - Process thousands/millions of entities efficiently

## Common Non-Game Use Cases

### 1. Simulations and Scientific Computing

```rust
use hecs::*;

// Weather simulation
struct Position { x: f64, y: f64, z: f64 }
struct Temperature(f64);
struct Humidity(f64);
struct WindVector { dx: f64, dy: f64, dz: f64 }

fn update_weather(world: &mut World) {
    // Process millions of air parcels efficiently
    for (id, (pos, temp, wind)) in world.query::<(&Position, &mut Temperature, &WindVector)>().iter() {
        // Update temperature based on position and wind
    }
}
```

### 2. Data Processing Pipelines

```rust
// ETL pipeline
struct RawData(String);
struct Parsed(Value);
struct Validated(Value);
struct Transformed(Output);
struct LoadReady;

fn parse_system(world: &mut World) {
    for (id, raw) in world.query::<&RawData>().iter() {
        if let Ok(parsed) = parse(&raw.0) {
            world.insert_one(id, Parsed(parsed)).ok();
            world.remove_one::<RawData>(id).ok();
        }
    }
}

fn validate_system(world: &mut World) {
    for (id, parsed) in world.query::<&Parsed>().iter() {
        if validate(&parsed.0) {
            world.insert_one(id, Validated(parsed.0.clone())).ok();
        }
    }
}
```

### 3. Network Services and Protocol Handling

```rust
// Connection management
struct TcpSocket(TcpStream);
struct UdpSocket(UdpSocket);
struct Authenticated { user: User };
struct RateLimited { tokens: u32, last_refill: Instant };
struct SessionData(HashMap<String, String>);

fn rate_limit_system(world: &mut World) {
    for (id, (limited, socket)) in world.query::<(&mut RateLimited, &TcpSocket)>().iter() {
        // Refill tokens over time
        if limited.last_refill.elapsed() > Duration::from_secs(1) {
            limited.tokens = limited.tokens.saturating_add(10).min(100);
            limited.last_refill = Instant::now();
        }
    }
}
```

### 4. UI and GUI Systems

```rust
// UI element composition
struct Position { x: i32, y: i32 }
struct Size { w: u32, h: u32 }
struct Visible;
struct Clickable { on_click: Box<dyn Fn()> }
struct Draggable { offset: (i32, i32) }
struct Text(String);
struct Background(Color);

// Flexible UI components through composition
fn create_button(world: &mut World, text: &str) -> Entity {
    world.spawn((
        Position { x: 0, y: 0 },
        Size { w: 100, h: 30 },
        Visible,
        Clickable { on_click: Box::new(|| println!("Clicked!")) },
        Text(text.into()),
        Background(Color::BLUE),
    ))
}
```

### 5. Business Process Workflows

```rust
// Order fulfillment system
struct Order { id: String, items: Vec<Item> }
struct PaymentPending;
struct PaymentReceived { transaction_id: String };
struct InventoryReserved;
struct Shipped { tracking: String };
struct Delivered;

fn payment_system(world: &mut World) {
    for (id, (order, _pending)) in world.query::<(&Order, &PaymentPending)>().iter() {
        if check_payment(&order.id) {
            world.remove_one::<PaymentPending>(id).ok();
            world.insert_one(id, PaymentReceived {
                transaction_id: get_transaction_id(&order.id)
            }).ok();
        }
    }
}

fn inventory_system(world: &mut World) {
    for (id, (order, _received)) in world.query::<(&Order, &PaymentReceived)>().iter() {
        if reserve_inventory(&order.items) {
            world.insert_one(id, InventoryReserved).ok();
        }
    }
}
```

## Popular Rust ECS Libraries

### hecs (Recommended for simplicity)
```rust
use hecs::*;

let mut world = World::new();

// Spawn entities
let entity = world.spawn((
    Position { x: 0.0, y: 0.0 },
    Velocity { dx: 1.0, dy: 0.0 },
));

// Query entities
for (id, (pos, vel)) in world.query::<(&mut Position, &Velocity)>().iter() {
    pos.x += vel.dx;
    pos.y += vel.dy;
}
```

**Pros:**
- Simple API
- Fast
- Good for learning
- Minimal boilerplate

**Cons:**
- Fewer features than bevy_ecs
- No built-in scheduling

### bevy_ecs (Full-featured)
```rust
use bevy_ecs::prelude::*;

let mut world = World::default();

world.spawn((
    Position { x: 0.0, y: 0.0 },
    Velocity { dx: 1.0, dy: 0.0 },
));

fn movement_system(mut query: Query<(&mut Position, &Velocity)>) {
    for (mut pos, vel) in query.iter_mut() {
        pos.x += vel.dx;
        pos.y += vel.dy;
    }
}
```

**Pros:**
- Rich feature set
- Excellent scheduling
- Change detection
- Events system
- Strong ecosystem

**Cons:**
- More complex
- Heavier dependency

### specs (Legacy, still maintained)
```rust
use specs::prelude::*;

struct Position { x: f32, y: f32 }
struct Velocity { dx: f32, dy: f32 }

impl Component for Position {
    type Storage = VecStorage<Self>;
}

impl Component for Velocity {
    type Storage = VecStorage<Self>;
}
```

**Use when:** Maintaining existing specs code

## Design Patterns

### Pattern: State Machines with Components

```rust
// Instead of enum State, use marker components
struct Idle;
struct Processing;
struct Complete;
struct Failed { error: String };

// Transition between states
fn start_processing(world: &mut World, entity: Entity) {
    world.remove_one::<Idle>(entity).ok();
    world.insert_one(entity, Processing).ok();
}
```

### Pattern: Tags for Filtering

```rust
// Use zero-size marker components as tags
struct Active;
struct Dirty;
struct NeedsUpdate;

// Efficient queries
for (id, data) in world.query::<&Data>()
    .with::<Active>()
    .with::<Dirty>()
    .iter()
{
    // Process only active, dirty entities
}
```

### Pattern: Hierarchical Relationships

```rust
struct Parent(Entity);
struct Children(Vec<Entity>);

fn propagate_transform(world: &World) {
    for (id, (transform, children)) in world.query::<(&Transform, &Children)>().iter() {
        for &child_id in &children.0 {
            if let Ok(mut child_transform) = world.get::<&mut Transform>(child_id) {
                // Apply parent transform to child
            }
        }
    }
}
```

### Pattern: Event Handling

```rust
struct Event<T> {
    data: T,
    timestamp: Instant,
}

struct MouseClick { x: i32, y: i32 }

// Spawn events as entities
fn emit_click(world: &mut World, x: i32, y: i32) {
    world.spawn((
        Event { data: MouseClick { x, y }, timestamp: Instant::now() },
    ));
}

// Process and cleanup events
fn handle_clicks(world: &mut World) {
    let to_remove: Vec<_> = world
        .query::<&Event<MouseClick>>()
        .iter()
        .map(|(id, _)| id)
        .collect();

    for id in to_remove {
        world.despawn(id).ok();
    }
}
```

## When NOT to Use ECS

- **Simple CRUD apps** - Traditional OOP/structs simpler
- **Small entity counts** - Overhead not worth it
- **Mostly static composition** - Just use structs with traits
- **Heavy entity relationships** - Graph databases better
- **Learning project** - Adds complexity

## When to Consider ECS

- **Dynamic composition needs** - Add/remove behavior at runtime
- **Many entities** (1000+) - Data-oriented benefits
- **Parallel processing** - ECS naturally parallelizable
- **Plugin architecture** - Systems as plugins
- **Simulation/emulation** - Natural fit

## Migration Strategy

Start with traditional Rust:
```rust
struct User {
    id: UserId,
    name: String,
    email: Email,
}
```

Add traits for behavior:
```rust
trait Notifiable {
    fn notify(&self, msg: &str);
}
```

If you need:
- Runtime behavior changes
- Massive scale
- Complex filtering

Then consider ECS:
```rust
struct UserId(u64);
struct Name(String);
struct Email(String);
struct Notifiable;

// Systems process entities with specific components
```

## Real-World Examples

**Successful non-game ECS usage:**
- Network packet processing (millions/sec)
- IoT device management (millions of devices)
- Scientific simulations (particle systems)
- Log aggregation pipelines
- Distributed tracing systems
- Real-time analytics
- Robotics control systems

## Performance Considerations

**ECS wins:**
- Cache-friendly iteration
- SIMD opportunities
- Parallel system execution
- Memory efficiency at scale

**ECS loses:**
- Random entity access
- Deep hierarchies
- Complex entity relationships
- Small datasets (overhead dominates)

## Philosophical Note

ECS is **data-oriented design**. Think:
- What data do I have?
- What operations do I perform?
- How do I organize for cache coherency?

Not:
- What objects do I have?
- What methods do they have?

This mindset shift unlocks ECS benefits beyond games.
