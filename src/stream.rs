//! Chunk streaming: decides which chunks should exist, generates them on worker
//! threads, and picks what to draw.
//!
//! The desired set is an octree cut: a sphere around the camera where chunks
//! double in size at each distance band. Every leaf is built independently, so
//! workers never wait on neighbours.

use crate::mesher::{self, Mesh, MeshScratch};
use crate::render::pool::RangeAllocator;
use crate::world::generator::{ChunkLook, Generator, Scratch, is_uniform};
use crate::world::{CHUNK_SIZE, ChunkKey, MAX_LOD, PADDED_VOLUME, Voxels, block_to_chunk, pidx};
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::JoinHandle;

/// Radius of the full-detail region, in blocks. Doubles at each LOD level.
pub const LOD0_RADIUS: f64 = 384.0;
/// Outer limit of the world around the player.
pub const VIEW_RADIUS: f64 = 4200.0;

/// Replanning walks the whole octree, so it's rate-limited even at high speed.
const PLAN_INTERVAL: f64 = 0.15;
/// New chunks become visible at the next selection. Rate-limited for the same reason.
const SELECT_INTERVAL: f64 = 0.08;

/// Should this node of the octree be subdivided for a camera at `cam`?
#[inline]
fn should_split(key: ChunkKey, cam: [f64; 3]) -> bool {
    if key.lod == 0 {
        return false;
    }
    let limit = LOD0_RADIUS * (1u64 << (key.lod - 1)) as f64;
    key.dist2(cam) < limit * limit
}

/// Top-level chunks that touch the view sphere.
fn for_each_root(cam: [f64; 3], mut f: impl FnMut(ChunkKey)) {
    let size = ((CHUNK_SIZE as i64) << MAX_LOD) as f64;
    let n = (VIEW_RADIUS / size).ceil() as i32;
    let cx = (cam[0] / size).floor() as i32;
    let cy = (cam[1] / size).floor() as i32;
    let cz = (cam[2] / size).floor() as i32;
    for y in cy - n..=cy + n {
        for z in cz - n..=cz + n {
            for x in cx - n..=cx + n {
                let k = ChunkKey::new(x, y, z, MAX_LOD);
                if k.dist2(cam) < VIEW_RADIUS * VIEW_RADIUS {
                    f(k);
                }
            }
        }
    }
}

/// All chunks that should be loaded for a camera position.
pub fn desired_leaves(cam: [f64; 3], out: &mut Vec<ChunkKey>) {
    out.clear();
    fn walk(k: ChunkKey, cam: [f64; 3], out: &mut Vec<ChunkKey>) {
        if should_split(k, cam) {
            for c in k.children() {
                walk(c, cam, out);
            }
        } else {
            out.push(k);
        }
    }
    for_each_root(cam, |k| walk(k, cam, out));
}

/// Finished chunk coming back from a worker.
pub struct Built {
    pub key: ChunkKey,
    pub world: u64,
    pub mesh: Mesh,
    pub voxels: Option<Box<Voxels>>,
    pub uniform: Option<u8>,
    pub look: ChunkLook,
    pub light: [u8; 125],
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Job {
    priority: u64,
    key: ChunkKey,
    world: u64,
}

impl Ord for Job {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        // BinaryHeap pops the largest, and lower numbers are more urgent.
        o.priority.cmp(&self.priority)
    }
}
impl PartialOrd for Job {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

/// Player edits, kept apart from generation. Chunks read this when they are
/// rebuilt so that changes survive unloading.
pub type EditMap = FxHashMap<(i32, i32, i32), FxHashMap<(i16, i16, i16), u8>>;

struct Shared {
    queue: Mutex<BinaryHeap<Job>>,
    wake: Condvar,
    quit: AtomicBool,
    generator: RwLock<Arc<Generator>>,
    edits: RwLock<EditMap>,
    busy_ns: AtomicU64,
    jobs_done: AtomicU64,
}

/// Applies recorded edits to a padded voxel array, including those made in
/// neighbouring chunks that fall inside the border.
fn apply_edits(edits: &EditMap, key: ChunkKey, v: &mut Voxels) {
    if edits.is_empty() || key.lod != 0 {
        return;
    }
    for dz in -1..=1 {
        for dy in -1..=1 {
            for dx in -1..=1 {
                let Some(map) = edits.get(&(key.x + dx, key.y + dy, key.z + dz)) else {
                    continue;
                };
                for (&(lx, ly, lz), &b) in map {
                    let px = lx as i32 + dx * CHUNK_SIZE + 1;
                    let py = ly as i32 + dy * CHUNK_SIZE + 1;
                    let pz = lz as i32 + dz * CHUNK_SIZE + 1;
                    if (0..34).contains(&px) && (0..34).contains(&py) && (0..34).contains(&pz) {
                        v[pidx(px as usize, py as usize, pz as usize)] = b;
                    }
                }
            }
        }
    }
}

fn worker(shared: Arc<Shared>, tx: Sender<Built>) {
    let mut gen_sc = Scratch::default();
    let mut mesh_sc = MeshScratch::default();
    let mut voxels: Box<Voxels> = vec![0u8; PADDED_VOLUME]
        .into_boxed_slice()
        .try_into()
        .unwrap();
    loop {
        let job = {
            let mut q = shared.queue.lock().unwrap();
            loop {
                if shared.quit.load(Ordering::Relaxed) {
                    return;
                }
                if let Some(j) = q.pop() {
                    break j;
                }
                q = shared.wake.wait(q).unwrap();
            }
        };
        let t0 = std::time::Instant::now();
        let generator = shared.generator.read().unwrap().clone();
        if generator.seed != job.world {
            continue;
        }
        generator.generate(job.key, &mut gen_sc, &mut voxels);
        if job.key.lod == 0 {
            apply_edits(&shared.edits.read().unwrap(), job.key, &mut voxels);
        }
        let uniform = is_uniform(&voxels);
        let mesh = if uniform.is_some() {
            Mesh::default()
        } else {
            mesher::build_mesh(&voxels, job.key.lod, &mut mesh_sc)
        };
        // Full-detail chunks keep their voxels for raycasts and edits. Uniform ones
        // stay represented by a single byte, and coarse ones need none at all.
        let kept = if job.key.lod == 0 && uniform.is_none() {
            Some(voxels.clone())
        } else {
            None
        };
        let light = if mesh.is_empty() {
            [0; 125]
        } else {
            generator.light_grid(job.key)
        };
        let look = generator.chunk_look(job.key);
        shared
            .busy_ns
            .fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
        shared.jobs_done.fetch_add(1, Ordering::Relaxed);
        let built = Built {
            key: job.key,
            world: job.world,
            mesh,
            voxels: kept,
            uniform,
            look,
            light,
        };
        if tx.send(built).is_err() {
            return;
        }
    }
}

/// A chunk known to the main thread.
pub struct Loaded {
    pub key: ChunkKey,
    /// GPU quads, as (first quad, quad count). `None` when there is nothing to draw.
    pub alloc: Option<(u32, u32)>,
    pub ranges: [(u32, u32); 7],
    pub slot: u32,
    pub bounds_min: [u8; 3],
    pub bounds_max: [u8; 3],
    pub voxels: Option<Box<Voxels>>,
    pub uniform: u8,
    pub in_use: bool,
}

/// What the renderer is told to do with GPU memory after a streaming step.
pub trait ChunkSink {
    fn write_quads(&mut self, first_quad: u32, quads: &[mesher::Quad]);
    fn write_chunk_info(&mut self, slot: u32, look: &ChunkLook, light: &[u8; 125], lod: u8);
}

pub struct Streamer {
    shared: Arc<Shared>,
    rx: Receiver<Built>,
    handles: Vec<JoinHandle<()>>,
    pub map: FxHashMap<ChunkKey, u32>,
    pub slab: Vec<Option<Loaded>>,
    free_ids: Vec<u32>,
    free_slots: Vec<u32>,
    next_slot: u32,
    pool: RangeAllocator,
    /// Everything submitted to the workers and not yet received: queued or running.
    pending: FxHashSet<ChunkKey>,
    desired: Vec<ChunkKey>,
    desired_set: FxHashSet<ChunkKey>,
    /// Slab ids of chunks to draw, nearest first.
    pub selected: Vec<u32>,
    last_plan_cam: [f64; 3],
    selection_dirty: bool,
    plan_cooldown: f64,
    select_cooldown: f64,
    pub stats: StreamStats,
    max_slots: u32,
    world: u64,
    generator: Arc<Generator>,
    voxel_scratch: MeshScratch,
}

#[derive(Default, Clone, Copy, Debug)]
pub struct StreamStats {
    pub desired: usize,
    pub loaded: usize,
    pub queued: usize,
    pub in_flight: usize,
    pub meshes: usize,
    pub used_quads: u64,
    pub jobs_done: u64,
    pub avg_job_us: f32,
    /// Worst main-thread cost of each stage since the last reset, in microseconds.
    pub plan_us: u32,
    pub accept_us: u32,
    pub select_us: u32,
}

impl Streamer {
    pub fn new(seed: u64, workers: usize, pool_quads: u32, max_slots: u32) -> Self {
        let generator = Arc::new(Generator::new(seed));
        let shared = Arc::new(Shared {
            queue: Mutex::new(BinaryHeap::new()),
            wake: Condvar::new(),
            quit: AtomicBool::new(false),
            generator: RwLock::new(generator.clone()),
            edits: RwLock::new(EditMap::default()),
            busy_ns: AtomicU64::new(0),
            jobs_done: AtomicU64::new(0),
        });
        let (tx, rx) = channel();
        let handles = (0..workers.max(1))
            .map(|i| {
                let s = shared.clone();
                let t = tx.clone();
                std::thread::Builder::new()
                    .name(format!("chunk-{i}"))
                    .spawn(move || worker(s, t))
                    .expect("spawn worker")
            })
            .collect();
        Self {
            shared,
            rx,
            handles,
            map: FxHashMap::default(),
            slab: Vec::new(),
            free_ids: Vec::new(),
            free_slots: Vec::new(),
            next_slot: 0,
            pool: RangeAllocator::new(pool_quads),
            pending: FxHashSet::default(),
            desired: Vec::new(),
            desired_set: FxHashSet::default(),
            selected: Vec::new(),
            last_plan_cam: [f64::MAX; 3],
            selection_dirty: true,
            plan_cooldown: 0.0,
            select_cooldown: 0.0,
            stats: StreamStats::default(),
            max_slots,
            world: seed,
            generator,
            voxel_scratch: MeshScratch::default(),
        }
    }

    pub fn generator(&self) -> &Arc<Generator> {
        &self.generator
    }

    /// Forgets everything and starts a new world from `seed`.
    pub fn reset(&mut self, seed: u64) {
        self.generator = Arc::new(Generator::new(seed));
        *self.shared.generator.write().unwrap() = self.generator.clone();
        self.shared.edits.write().unwrap().clear();
        self.shared.queue.lock().unwrap().clear();
        self.world = seed;
        self.map.clear();
        self.slab.clear();
        self.free_ids.clear();
        self.free_slots.clear();
        self.next_slot = 0;
        self.pool.clear();
        self.pending.clear();
        self.desired.clear();
        self.desired_set.clear();
        self.selected.clear();
        self.last_plan_cam = [f64::MAX; 3];
        self.selection_dirty = true;
        self.plan_cooldown = 0.0;
        self.select_cooldown = 0.0;
    }

    /// Advances streaming: replans when the camera moved, takes in finished
    /// chunks within the time budget, and refreshes the draw selection.
    pub fn update(
        &mut self,
        cam: [f64; 3],
        view_dir: [f32; 3],
        dt: f64,
        budget_us: u64,
        sink: &mut impl ChunkSink,
    ) {
        let dx = cam[0] - self.last_plan_cam[0];
        let dy = cam[1] - self.last_plan_cam[1];
        let dz = cam[2] - self.last_plan_cam[2];
        self.plan_cooldown -= dt;
        self.select_cooldown -= dt;
        let moved = dx * dx + dy * dy + dz * dz > 12.0 * 12.0;
        if moved && self.plan_cooldown <= 0.0 {
            let t = std::time::Instant::now();
            self.plan(cam, view_dir);
            self.plan_cooldown = PLAN_INTERVAL;
            self.stats.plan_us = self.stats.plan_us.max(t.elapsed().as_micros() as u32);
        }

        let t0 = std::time::Instant::now();
        let mut uploaded = 0usize;
        loop {
            if t0.elapsed().as_micros() as u64 > budget_us {
                break;
            }
            let Ok(built) = self.rx.try_recv() else {
                break;
            };
            if built.world != self.world {
                continue;
            }
            self.pending.remove(&built.key);
            // The camera moved on while this was being built. Replanning requeues it
            // if it becomes relevant again.
            if !self.desired_set.contains(&built.key) {
                continue;
            }
            uploaded += self.accept(built, sink);
            if uploaded > 64 * 1024 * 1024 {
                break;
            }
        }

        self.stats.accept_us = self.stats.accept_us.max(t0.elapsed().as_micros() as u32);

        if self.selection_dirty && self.select_cooldown <= 0.0 {
            let t = std::time::Instant::now();
            self.reselect(cam);
            self.selection_dirty = false;
            self.select_cooldown = SELECT_INTERVAL;
            self.stats.select_us = self.stats.select_us.max(t.elapsed().as_micros() as u32);
        }

        self.stats.desired = self.desired.len();
        self.stats.loaded = self.map.len();
        self.stats.queued = self.shared.queue.lock().unwrap().len();
        self.stats.in_flight = self.pending.len().saturating_sub(self.stats.queued);
        self.stats.used_quads = self.pool.used() as u64;
        let done = self.shared.jobs_done.load(Ordering::Relaxed);
        self.stats.jobs_done = done;
        if done > 0 {
            self.stats.avg_job_us =
                (self.shared.busy_ns.load(Ordering::Relaxed) as f64 / done as f64 / 1000.0) as f32;
        }
    }

    /// True when everything the camera wants has been generated and uploaded.
    pub fn is_settled(&self) -> bool {
        self.pending.is_empty() && self.stats.desired > 0
    }

    fn plan(&mut self, cam: [f64; 3], view_dir: [f32; 3]) {
        self.last_plan_cam = cam;
        desired_leaves(cam, &mut self.desired);
        self.desired_set.clear();
        self.desired_set.extend(self.desired.iter().copied());

        let mut queue = self.shared.queue.lock().unwrap();
        // Jobs nobody has started are withdrawn and reprioritized. Those still in
        // `pending` afterwards are running, so they aren't submitted twice.
        for job in queue.drain() {
            self.pending.remove(&job.key);
        }
        for &key in &self.desired {
            if self.map.contains_key(&key) || !self.pending.insert(key) {
                continue;
            }
            queue.push(Job {
                priority: job_priority(key, cam, view_dir),
                key,
                world: self.world,
            });
        }
        drop(queue);
        self.shared.wake.notify_all();

        self.selection_dirty = true;
    }

    /// Registers a built chunk. Returns the number of bytes uploaded.
    fn accept(&mut self, b: Built, sink: &mut impl ChunkSink) -> usize {
        // A chunk rebuilt while a replacement already exists keeps the newest.
        if let Some(&id) = self.map.get(&b.key) {
            self.remove_id(id);
        }
        let (alloc, slot) = if b.mesh.is_empty() {
            (None, u32::MAX)
        } else {
            let Some(first) = self.pool.alloc(b.mesh.quads.len() as u32) else {
                log::warn!("quad pool is full, dropping chunk {:?}", b.key);
                return 0;
            };
            let Some(slot) = self.take_slot() else {
                self.pool.free(first, b.mesh.quads.len() as u32);
                log::warn!("chunk slots exhausted, dropping chunk {:?}", b.key);
                return 0;
            };
            sink.write_quads(first, &b.mesh.quads);
            sink.write_chunk_info(slot, &b.look, &b.light, b.key.lod);
            (Some((first, b.mesh.quads.len() as u32)), slot)
        };
        let bytes = b.mesh.quads.len() * 8;
        let id = self.free_ids.pop().unwrap_or_else(|| {
            self.slab.push(None);
            self.slab.len() as u32 - 1
        });
        self.slab[id as usize] = Some(Loaded {
            key: b.key,
            alloc,
            ranges: b.mesh.ranges,
            slot,
            bounds_min: b.mesh.bounds_min,
            bounds_max: b.mesh.bounds_max,
            voxels: b.voxels,
            uniform: b.uniform.unwrap_or(STONE_FALLBACK),
            in_use: false,
        });
        self.map.insert(b.key, id);
        self.selection_dirty = true;
        bytes
    }

    fn take_slot(&mut self) -> Option<u32> {
        if let Some(s) = self.free_slots.pop() {
            return Some(s);
        }
        if self.next_slot < self.max_slots {
            self.next_slot += 1;
            Some(self.next_slot - 1)
        } else {
            None
        }
    }

    fn remove_id(&mut self, id: u32) {
        if let Some(l) = self.slab[id as usize].take() {
            if let Some((first, count)) = l.alloc {
                self.pool.free(first, count);
                self.free_slots.push(l.slot);
            }
            self.map.remove(&l.key);
            self.free_ids.push(id);
        }
    }

    /// Picks what to draw, falling back to whatever is loaded (coarser or finer)
    /// while the ideal chunks are still being built, so no holes appear. Chunks
    /// that are neither wanted nor used as a fallback are then evicted.
    fn reselect(&mut self, cam: [f64; 3]) {
        for l in self.slab.iter_mut().flatten() {
            l.in_use = false;
        }
        self.selected.clear();
        let mut chosen = std::mem::take(&mut self.selected);
        let this = &mut *self;
        for_each_root(cam, |k| {
            this.select_node(k, cam, &mut chosen);
        });
        self.selected = chosen;

        for &id in &self.selected {
            if let Some(l) = self.slab[id as usize].as_mut() {
                l.in_use = true;
            }
        }
        let mut doomed = Vec::new();
        for (id, l) in self.slab.iter().enumerate() {
            if let Some(l) = l
                && !l.in_use
                && !self.desired_set.contains(&l.key)
            {
                doomed.push(id as u32);
            }
        }
        for id in doomed {
            self.remove_id(id);
        }

        let slab = &self.slab;
        self.selected.sort_by_cached_key(|&id| {
            let k = slab[id as usize].as_ref().unwrap().key;
            k.dist2(cam) as u64
        });
        self.stats.meshes = self.selected.len();
    }

    /// Returns true when this node's whole volume is covered by what `out` got.
    fn select_node(&self, k: ChunkKey, cam: [f64; 3], out: &mut Vec<u32>) -> bool {
        let loaded = self.map.get(&k).copied();
        if !should_split(k, cam) {
            if let Some(id) = loaded {
                push_if_mesh(&self.slab, id, out);
                return true;
            }
            // Wanted but missing: show finer leftovers if they cover it entirely.
            if k.lod > 0 {
                return self.select_any_finer(k, out);
            }
            return false;
        }
        let mark = out.len();
        let mut covered = true;
        for c in k.children() {
            covered &= self.select_node(c, cam, out);
        }
        if covered {
            return true;
        }
        // Part of the finer version is missing: use this coarser node instead.
        if let Some(id) = loaded {
            out.truncate(mark);
            push_if_mesh(&self.slab, id, out);
            return true;
        }
        false
    }

    fn select_any_finer(&self, k: ChunkKey, out: &mut Vec<u32>) -> bool {
        let mark = out.len();
        for c in k.children() {
            let covered = if let Some(&id) = self.map.get(&c) {
                push_if_mesh(&self.slab, id, out);
                true
            } else {
                c.lod > 0 && self.select_any_finer(c, out)
            };
            // One gap makes this attempt useless, so stop before visiting the rest of
            // what could be thousands of descendants.
            if !covered {
                out.truncate(mark);
                return false;
            }
        }
        true
    }

    /// Reads a block from full-detail data. `None` when that chunk isn't loaded.
    pub fn block_at(&self, x: i64, y: i64, z: i64) -> Option<u8> {
        let k = ChunkKey::new(block_to_chunk(x), block_to_chunk(y), block_to_chunk(z), 0);
        let id = *self.map.get(&k)?;
        let l = self.slab[id as usize].as_ref()?;
        match &l.voxels {
            Some(v) => {
                let lx = x.rem_euclid(CHUNK_SIZE as i64) as usize + 1;
                let ly = y.rem_euclid(CHUNK_SIZE as i64) as usize + 1;
                let lz = z.rem_euclid(CHUNK_SIZE as i64) as usize + 1;
                Some(v[pidx(lx, ly, lz)])
            }
            None => Some(l.uniform),
        }
    }

    /// Changes a block and rebuilds every affected mesh in place, right away.
    /// Returns the number of chunks remeshed, or `None` if the block isn't loaded.
    pub fn set_block(
        &mut self,
        x: i64,
        y: i64,
        z: i64,
        block: u8,
        sink: &mut impl ChunkSink,
    ) -> Option<usize> {
        let (cx, cy, cz) = (block_to_chunk(x), block_to_chunk(y), block_to_chunk(z));
        let lx = x.rem_euclid(CHUNK_SIZE as i64) as i32;
        let ly = y.rem_euclid(CHUNK_SIZE as i64) as i32;
        let lz = z.rem_euclid(CHUNK_SIZE as i64) as i32;
        self.map.get(&ChunkKey::new(cx, cy, cz, 0))?;

        self.shared
            .edits
            .write()
            .unwrap()
            .entry((cx, cy, cz))
            .or_default()
            .insert((lx as i16, ly as i16, lz as i16), block);

        // Every chunk whose border contains this block must also change, since
        // the border drives both face culling and AO.
        let ranges = |l: i32| -> &'static [i32] {
            if l == 0 {
                &[0, -1]
            } else if l == CHUNK_SIZE - 1 {
                &[0, 1]
            } else {
                &[0]
            }
        };
        let mut touched = 0;
        for &dy in ranges(ly) {
            for &dz in ranges(lz) {
                for &dx in ranges(lx) {
                    let key = ChunkKey::new(cx + dx, cy + dy, cz + dz, 0);
                    let px = (lx - dx * CHUNK_SIZE + 1) as usize;
                    let py = (ly - dy * CHUNK_SIZE + 1) as usize;
                    let pz = (lz - dz * CHUNK_SIZE + 1) as usize;
                    if self.edit_loaded_chunk(key, pidx(px, py, pz), block, sink) {
                        touched += 1;
                    }
                }
            }
        }
        Some(touched)
    }

    fn edit_loaded_chunk(
        &mut self,
        key: ChunkKey,
        index: usize,
        block: u8,
        sink: &mut impl ChunkSink,
    ) -> bool {
        let Some(&id) = self.map.get(&key) else {
            return false;
        };
        let l = self.slab[id as usize].as_mut().unwrap();
        // Uniform chunks hold no voxels, so they get them on first edit.
        let voxels = l.voxels.get_or_insert_with(|| {
            vec![l.uniform; PADDED_VOLUME]
                .into_boxed_slice()
                .try_into()
                .unwrap()
        });
        if voxels[index] == block {
            return false;
        }
        voxels[index] = block;
        let mesh = mesher::build_mesh(voxels, 0, &mut self.voxel_scratch);
        if let Some((first, count)) = l.alloc.take() {
            self.pool.free(first, count);
        }
        let mut new_alloc = None;
        if !mesh.is_empty() {
            let count = mesh.quads.len() as u32;
            match self.pool.alloc(count) {
                Some(first) => {
                    sink.write_quads(first, &mesh.quads);
                    new_alloc = Some((first, count));
                }
                None => log::warn!("quad pool is full while editing {key:?}"),
            }
        }
        let had_slot = l.slot != u32::MAX;
        l.alloc = new_alloc;
        l.ranges = mesh.ranges;
        l.bounds_min = mesh.bounds_min;
        l.bounds_max = mesh.bounds_max;
        if new_alloc.is_some() && !had_slot {
            let slot = self.take_slot().unwrap_or(0);
            let look = self.generator.chunk_look(key);
            let light = self.generator.light_grid(key);
            sink.write_chunk_info(slot, &look, &light, 0);
            self.slab[id as usize].as_mut().unwrap().slot = slot;
        } else if new_alloc.is_none() && had_slot {
            self.free_slots.push(l.slot);
            l.slot = u32::MAX;
        }
        // Only a chunk that gained its first mesh is new to the draw list. It shouldn't
        // wait for the rate limit, or the hole would stay see-through for a moment.
        if !had_slot {
            self.selection_dirty = true;
            self.select_cooldown = 0.0;
        }
        true
    }

    pub fn shutdown(&mut self) {
        self.shared.quit.store(true, Ordering::Relaxed);
        self.shared.wake.notify_all();
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
    }
}

const STONE_FALLBACK: u8 = crate::world::block::STONE;

impl Drop for Streamer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[inline]
fn push_if_mesh(slab: &[Option<Loaded>], id: u32, out: &mut Vec<u32>) {
    if let Some(l) = &slab[id as usize]
        && l.alloc.is_some()
    {
        out.push(id);
    }
}

/// Lower numbers build first. Distance is measured relative to chunk size, so
/// coarse far chunks arrive quickly and give an instant vista, and chunks in
/// front of the camera beat those behind it.
fn job_priority(key: ChunkKey, cam: [f64; 3], view: [f32; 3]) -> u64 {
    let o = key.origin();
    let half = key.size() as f64 * 0.5;
    let d = [
        o[0] as f64 + half - cam[0],
        o[1] as f64 + half - cam[1],
        o[2] as f64 + half - cam[2],
    ];
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1.0);
    let facing = (d[0] * view[0] as f64 + d[1] * view[1] as f64 + d[2] * view[2] as f64) / len;
    let bias = 1.0 - 0.45 * facing.max(0.0);
    let rel = len / (1u64 << key.lod) as f64;
    (rel * bias * 16.0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Marks each level-0-equivalent cell of a leaf, for overlap checks.
    fn volume(k: ChunkKey) -> f64 {
        (k.size() as f64).powi(3)
    }

    #[test]
    fn leaves_do_not_overlap_and_fill_the_sphere() {
        for cam in [
            [0.5, 0.5, 0.5],
            [1234.5, -987.0, 4567.0],
            [-15.0, 31.9, 32.1],
        ] {
            let mut leaves = Vec::new();
            desired_leaves(cam, &mut leaves);
            let set: FxHashSet<_> = leaves.iter().copied().collect();
            assert_eq!(set.len(), leaves.len(), "duplicate leaves");

            // No leaf may have an ancestor that is also a leaf.
            for k in &leaves {
                let mut p = *k;
                while p.lod < MAX_LOD {
                    p = ChunkKey::new(
                        p.x.div_euclid(2),
                        p.y.div_euclid(2),
                        p.z.div_euclid(2),
                        p.lod + 1,
                    );
                    assert!(!set.contains(&p), "{k:?} overlaps its ancestor {p:?}");
                }
            }
            // The leaves must cover at least the whole view sphere.
            let total: f64 = leaves.iter().map(|k| volume(*k)).sum();
            let sphere = 4.0 / 3.0 * std::f64::consts::PI * VIEW_RADIUS.powi(3);
            assert!(
                total >= sphere,
                "leaf volume {total} under sphere volume {sphere}"
            );
        }
    }

    #[test]
    fn detail_shrinks_with_distance() {
        let cam = [10.0, 10.0, 10.0];
        let mut leaves = Vec::new();
        desired_leaves(cam, &mut leaves);
        let mut count = [0usize; 5];
        for k in &leaves {
            count[k.lod as usize] += 1;
            let d = k.dist2(cam).sqrt();
            if k.lod == 0 {
                // A chunk may stick out past the radius, but only if its parent is inside.
                let parent =
                    ChunkKey::new(k.x.div_euclid(2), k.y.div_euclid(2), k.z.div_euclid(2), 1);
                assert!(
                    parent.dist2(cam).sqrt() < LOD0_RADIUS,
                    "full detail chunk too far: {d}"
                );
            } else {
                // Coarse chunks must stay outside the band that demands finer ones.
                let limit = LOD0_RADIUS * (1u64 << (k.lod - 1)) as f64;
                assert!(d >= limit, "LOD {} at distance {d}", k.lod);
            }
        }
        assert!(count.iter().all(|&c| c > 0), "every level used: {count:?}");
        // A sanity bound on the memory and job count of the whole sphere.
        assert!(leaves.len() < 45_000, "too many leaves: {}", leaves.len());
    }
}
