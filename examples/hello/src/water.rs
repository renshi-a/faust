//! A ball of water that ripples as the person types. The water is simulated on the GPU with the
//! material point method and drawn as a lit, refracting surface; the shaders are in
//! `water.metal`, which Metal compiles when the app starts.
//!
//! Metal is part of every Mac. Like AppKit, it is loaded at run time and reached through
//! Objective-C messages, so the build needs no SDK and no Xcode.

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, c_char, c_void};
use std::ptr::{null, null_mut};

use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, Bool, NSObject};
use objc2::{
    DefinedClass, Encode, Encoding, MainThreadMarker, MainThreadOnly, class, define_class, extern_class, msg_send, sel,
};

use crate::{Id, Rect, Size, nsstring};

const SHADERS: &str = include_str!("water.metal");
/// The simulation grid's side, in cells.
const GRID: u32 = 64;
/// The ball's radius at rest, in cells, and how far apart its drops start.
const RADIUS: f32 = 13.0;
const SPACING: f32 = 0.5;
/// Simulation steps per frame, and their length.
const SUBSTEPS: usize = 3;
const DT: f32 = 0.16;
/// One drop's size in a `Particle` (see `water.metal`): two `float3`s and a `float3x3`.
const PARTICLE: usize = 80;
const THREADS: usize = 64;

// Metal's enumerations, as the headers number them.
const BGRA8_SRGB: usize = 81;
const R32_FLOAT: usize = 55;
const R16_FLOAT: usize = 25;
const DEPTH32_FLOAT: usize = 252;
const LOAD_DONT_CARE: usize = 0;
const LOAD_CLEAR: usize = 2;
const STORE_DONT_CARE: usize = 0;
const STORE: usize = 1;
const TRIANGLE: usize = 3;
const TRIANGLE_STRIP: usize = 4;
const COMPARE_LESS: usize = 1;
const BLEND_ONE: usize = 1;
const USAGE_READ_WRITE_TARGET: usize = 1 | 2 | 4;
const STORAGE_PRIVATE: usize = 2;

#[repr(C)]
#[derive(Clone, Copy)]
struct MtlSize {
    width: usize,
    height: usize,
    depth: usize,
}

// SAFETY: the layout matches MTLSize (three NSUIntegers).
unsafe impl Encode for MtlSize {
    const ENCODING: Encoding = Encoding::Struct("?", &[usize::ENCODING, usize::ENCODING, usize::ENCODING]);
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ClearColor {
    r: f64,
    g: f64,
    b: f64,
    a: f64,
}

// SAFETY: the layout matches MTLClearColor (four doubles).
unsafe impl Encode for ClearColor {
    const ENCODING: Encoding = Encoding::Struct("?", &[f64::ENCODING, f64::ENCODING, f64::ENCODING, f64::ENCODING]);
}

/// `Sim` in `water.metal`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SimParams {
    centre: [f32; 4],
    impulse: [f32; 4],
    fluid: [f32; 4],
    misc: [f32; 4],
    swirl: [f32; 4],
    tide: [f32; 4],
    size: [u32; 4],
}

/// What pushes the water about in a frame, besides its own weight.
#[derive(Clone, Copy, Default)]
struct Forces {
    /// A ripple: where it starts (grid cells) and how hard.
    impulse: [f32; 4],
    /// Every drop thrown outward, this hard.
    burst: f32,
    /// The tide: the axis the water is drawn out along, and how hard.
    tide: [f32; 4],
    /// The turn: its axis, and how fast.
    swirl: [f32; 4],
}

/// The water's own motion `t` seconds in: a tide whose axis circles the ball and whose pull
/// swells and fades, and a slow turn about an axis that wanders. The rates never line up, so
/// the ball keeps changing shape without end.
fn motion(t: f32) -> ([f32; 4], [f32; 4]) {
    let along = normalize([(t * 0.7).cos(), 0.6 * (t * 0.43).sin(), (t * 0.7).sin()]);
    let tide = [along[0], along[1], along[2], 0.0009 * (0.6 + 0.4 * (t * 1.1).sin())];
    let axis = normalize([(t * 0.23).sin(), 1.0, (t * 0.31).cos()]);
    (tide, [axis[0], axis[1], axis[2], 0.022 * (t * 0.2).sin()])
}

type Mat = [[f32; 4]; 4];

/// `View` in `water.metal`. Matrices are column by column, as Metal keeps them.
#[repr(C)]
#[derive(Clone, Copy)]
struct ViewParams {
    view: Mat,
    proj: Mat,
    inv_view: Mat,
    inv_proj: Mat,
    screen: [f32; 4],
    drop: [f32; 4],
    sun: [f32; 4],
    eye: [f32; 4],
    water: [f32; 4],
}

// ---------------------------------------------------------------------------------------------
// Matrices

#[cfg(test)]
fn mul(a: &Mat, b: &Mat) -> Mat {
    let mut m = [[0.0; 4]; 4];
    for (c, col) in m.iter_mut().enumerate() {
        for (r, v) in col.iter_mut().enumerate() {
            *v = (0..4).map(|k| a[k][r] * b[c][k]).sum();
        }
    }
    m
}

/// The inverse, by Gauss-Jordan elimination; `None` when there is none.
fn inverse(m: &Mat) -> Option<Mat> {
    // Rows of [m | I], with m read as rows.
    let mut a = [[0.0f64; 8]; 4];
    for (r, row) in a.iter_mut().enumerate() {
        for c in 0..4 {
            row[c] = f64::from(m[c][r]);
        }
        row[4 + r] = 1.0;
    }
    for col in 0..4 {
        let pivot = (col..4).max_by(|&x, &y| a[x][col].abs().total_cmp(&a[y][col].abs()))?;
        if a[pivot][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, pivot);
        let p = a[col][col];
        a[col].iter_mut().for_each(|v| *v /= p);
        let pivot_row = a[col];
        for (r, row) in a.iter_mut().enumerate() {
            if r != col {
                let f = row[col];
                for (v, p) in row.iter_mut().zip(pivot_row) {
                    *v -= f * p;
                }
            }
        }
    }
    let mut out = [[0.0; 4]; 4];
    for (r, row) in a.iter().enumerate() {
        for c in 0..4 {
            out[c][r] = row[4 + c] as f32;
        }
    }
    Some(out)
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(a: [f32; 3]) -> [f32; 3] {
    let l = dot(a, a).sqrt().max(1e-12);
    [a[0] / l, a[1] / l, a[2] / l]
}

/// A camera at `eye` looking at `target`; it looks down its own -z.
fn look_at(eye: [f32; 3], target: [f32; 3]) -> Mat {
    let f = normalize(sub(target, eye));
    let s = normalize(cross(f, [0.0, 1.0, 0.0]));
    let u = cross(s, f);
    [
        [s[0], u[0], -f[0], 0.0],
        [s[1], u[1], -f[1], 0.0],
        [s[2], u[2], -f[2], 0.0],
        [-dot(s, eye), -dot(u, eye), dot(f, eye), 1.0],
    ]
}

/// Perspective into Metal's clip space (depth 0 to 1), with the centre moved up by `lift`
/// (a fraction of the height, as in clip space).
fn perspective(fov_y: f32, aspect: f32, lift: f32, near: f32, far: f32) -> Mat {
    let f = 1.0 / (fov_y / 2.0).tan();
    [
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, -lift, far / (near - far), -1.0],
        [0.0, 0.0, near * far / (near - far), 0.0],
    ]
}

// ---------------------------------------------------------------------------------------------
// Metal

fn load_frameworks() {
    unsafe extern "C" {
        fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
    }
    for path in [
        c"/System/Library/Frameworks/Metal.framework/Metal",
        c"/System/Library/Frameworks/QuartzCore.framework/QuartzCore",
    ] {
        // SAFETY: dlopen takes a NUL-terminated path; a missing framework returns null.
        unsafe { dlopen(path.as_ptr(), 2) };
    }
}

fn rust_string(s: *mut AnyObject) -> String {
    if s.is_null() {
        return String::new();
    }
    // SAFETY: -[NSString UTF8String] returns a NUL-terminated string that lives as long as `s`.
    unsafe {
        let p: *const c_char = msg_send![s, UTF8String];
        if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() }
    }
}

fn describe(error: *mut AnyObject) -> String {
    if error.is_null() {
        return "unknown error".into();
    }
    // SAFETY: -[NSError localizedDescription] returns an NSString.
    rust_string(unsafe { msg_send![error, localizedDescription] })
}

/// The GPU, and the water's shaders compiled for it.
struct Gpu {
    device: Id,
    queue: Id,
    library: Id,
}

impl Gpu {
    fn new() -> Result<Gpu, String> {
        unsafe extern "C" {
            fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        }
        load_frameworks();
        // SAFETY: dlsym with RTLD_DEFAULT and a NUL-terminated name; Metal is loaded by then.
        let create = unsafe { dlsym(-2isize as *mut c_void, c"MTLCreateSystemDefaultDevice".as_ptr()) };
        if create.is_null() {
            return Err("Metal isn't available".into());
        }
        // SAFETY: MTLCreateSystemDefaultDevice takes nothing and returns a +1 device, or nil.
        let device = unsafe {
            let create: extern "C" fn() -> *mut AnyObject = std::mem::transmute(create);
            Retained::from_raw(create())
        }
        .ok_or("this Mac has no Metal device")?;
        // SAFETY: MTLDevice's documented messages; the error is filled in only on failure.
        unsafe {
            let queue: Option<Id> = msg_send![&*device, newCommandQueue];
            let queue = queue.ok_or("no command queue")?;
            let mut error: *mut AnyObject = null_mut();
            let library: Option<Id> = msg_send![&*device, newLibraryWithSource: &*nsstring(SHADERS), options: null::<AnyObject>(), error: &mut error];
            let library = library.ok_or_else(|| format!("the shaders don't compile: {}", describe(error)))?;
            Ok(Gpu { device, queue, library })
        }
    }

    fn function(&self, name: &str) -> Result<Id, String> {
        // SAFETY: -[MTLLibrary newFunctionWithName:] returns a +1 function, or nil.
        let f: Option<Id> = unsafe { msg_send![&*self.library, newFunctionWithName: &*nsstring(name)] };
        f.ok_or_else(|| format!("the shaders have no {name}"))
    }

    fn compute(&self, name: &str) -> Result<Id, String> {
        let f = self.function(name)?;
        let mut error: *mut AnyObject = null_mut();
        // SAFETY: -[MTLDevice newComputePipelineStateWithFunction:error:].
        let p: Option<Id> =
            unsafe { msg_send![&*self.device, newComputePipelineStateWithFunction: &*f, error: &mut error] };
        p.ok_or_else(|| format!("{name}: {}", describe(error)))
    }

    /// A render pipeline for sprites or a full-screen pass, drawing into `format`.
    fn render(&self, vertex: &str, fragment: &str, format: usize, depth: bool, additive: bool) -> Result<Id, String> {
        let (v, f) = (self.function(vertex)?, self.function(fragment)?);
        let mut error: *mut AnyObject = null_mut();
        // SAFETY: MTLRenderPipelineDescriptor's documented setters, then the device builds it.
        unsafe {
            let d: Id = msg_send![class!(MTLRenderPipelineDescriptor), new];
            let _: () = msg_send![&*d, setVertexFunction: &*v];
            let _: () = msg_send![&*d, setFragmentFunction: &*f];
            let attachments: Id = msg_send![&*d, colorAttachments];
            let a: Id = msg_send![&*attachments, objectAtIndexedSubscript: 0usize];
            let _: () = msg_send![&*a, setPixelFormat: format];
            if additive {
                let _: () = msg_send![&*a, setBlendingEnabled: Bool::YES];
                let _: () = msg_send![&*a, setSourceRGBBlendFactor: BLEND_ONE];
                let _: () = msg_send![&*a, setDestinationRGBBlendFactor: BLEND_ONE];
                let _: () = msg_send![&*a, setSourceAlphaBlendFactor: BLEND_ONE];
                let _: () = msg_send![&*a, setDestinationAlphaBlendFactor: BLEND_ONE];
            }
            if depth {
                let _: () = msg_send![&*d, setDepthAttachmentPixelFormat: DEPTH32_FLOAT];
            }
            let p: Option<Id> = msg_send![&*self.device, newRenderPipelineStateWithDescriptor: &*d, error: &mut error];
            p.ok_or_else(|| format!("{vertex}/{fragment}: {}", describe(error)))
        }
    }

    fn buffer(&self, len: usize) -> Result<Id, String> {
        // SAFETY: -[MTLDevice newBufferWithLength:options:], shared storage.
        let b: Option<Id> = unsafe { msg_send![&*self.device, newBufferWithLength: len, options: 0usize] };
        b.ok_or_else(|| format!("no GPU memory for {len} bytes"))
    }

    fn texture(&self, format: usize, width: usize, height: usize) -> Result<Id, String> {
        // SAFETY: MTLTextureDescriptor's documented class method and setters.
        unsafe {
            let d: Id = msg_send![class!(MTLTextureDescriptor), texture2DDescriptorWithPixelFormat: format, width: width, height: height, mipmapped: Bool::NO];
            let _: () = msg_send![&*d, setUsage: USAGE_READ_WRITE_TARGET];
            let _: () = msg_send![&*d, setStorageMode: STORAGE_PRIVATE];
            let t: Option<Id> = msg_send![&*self.device, newTextureWithDescriptor: &*d];
            t.ok_or_else(|| "no GPU memory for a texture".into())
        }
    }
}

fn groups(n: usize) -> MtlSize {
    MtlSize { width: n.div_ceil(THREADS), height: 1, depth: 1 }
}

const LINE: MtlSize = MtlSize { width: THREADS, height: 1, depth: 1 };

// ---------------------------------------------------------------------------------------------
// The simulation

/// Drops laid out on a lattice inside the ball, at rest, in grid cells.
fn starting_drops() -> Vec<[f32; 3]> {
    let c = GRID as f32 / 2.0;
    let steps = (RADIUS / SPACING) as i32;
    let mut out = Vec::new();
    for i in -steps..=steps {
        for j in -steps..=steps {
            for k in -steps..=steps {
                let p = [i as f32 * SPACING, j as f32 * SPACING, k as f32 * SPACING];
                if dot(p, p) <= RADIUS * RADIUS {
                    out.push([c + p[0], c + p[1], c + p[2]]);
                }
            }
        }
    }
    out
}

struct Fluid {
    particles: Id,
    cells: Id,
    velocity: Id,
    count: usize,
    clear: Id,
    p2g_mass: Id,
    p2g_stress: Id,
    grid_update: Id,
    g2p: Id,
}

impl Fluid {
    fn new(gpu: &Gpu) -> Result<Fluid, String> {
        let drops = starting_drops();
        let cells = (GRID * GRID * GRID) as usize;
        let particles = gpu.buffer(drops.len() * PARTICLE)?;
        // SAFETY: the buffer is shared and holds `drops.len()` particles of PARTICLE bytes; the
        // GPU hasn't used it yet.
        unsafe {
            let base: *mut c_void = msg_send![&*particles, contents];
            let base = base.cast::<f32>();
            std::ptr::write_bytes(base, 0, drops.len() * PARTICLE / 4);
            for (i, d) in drops.iter().enumerate() {
                let p = base.add(i * PARTICLE / 4);
                p.copy_from_nonoverlapping(d.as_ptr(), 3);
            }
        }
        Ok(Fluid {
            particles,
            cells: gpu.buffer(cells * 16)?,
            velocity: gpu.buffer(cells * 16)?,
            count: drops.len(),
            clear: gpu.compute("clear_grid")?,
            p2g_mass: gpu.compute("p2g_mass")?,
            p2g_stress: gpu.compute("p2g_stress")?,
            grid_update: gpu.compute("grid_update")?,
            g2p: gpu.compute("g2p")?,
        })
    }

    /// One frame of simulation.
    fn encode(&self, cmd: &AnyObject, f: &Forces) {
        let c = GRID as f32 / 2.0;
        let params = SimParams {
            centre: [c, c, c, 0.05],
            impulse: f.impulse,
            fluid: [DT, 8.0, 5.0, 0.15],
            misc: [65_536.0, f.burst, 3.5, 0.0],
            swirl: f.swirl,
            tide: f.tide,
            size: [self.count as u32, GRID, 0, 0],
        };
        let cells = (GRID * GRID * GRID) as usize;
        // SAFETY: MTLComputeCommandEncoder's documented messages; every buffer outlives the
        // command buffer, and the parameters are copied when they are set.
        unsafe {
            let e: Id = msg_send![cmd, computeCommandEncoder];
            let set = |pipeline: &Id, buffers: &[&Id]| {
                let _: () = msg_send![&*e, setComputePipelineState: &**pipeline];
                for (i, b) in buffers.iter().enumerate() {
                    let _: () = msg_send![&*e, setBuffer: &***b, offset: 0usize, atIndex: i];
                }
                let _: () = msg_send![&*e, setBytes: (&raw const params).cast::<c_void>(), length: size_of::<SimParams>(), atIndex: buffers.len()];
            };
            for _ in 0..SUBSTEPS {
                set(&self.clear, &[&self.cells]);
                let _: () = msg_send![&*e, dispatchThreadgroups: groups(cells), threadsPerThreadgroup: LINE];
                set(&self.p2g_mass, &[&self.particles, &self.cells]);
                let _: () = msg_send![&*e, dispatchThreadgroups: groups(self.count), threadsPerThreadgroup: LINE];
                set(&self.p2g_stress, &[&self.particles, &self.cells]);
                let _: () = msg_send![&*e, dispatchThreadgroups: groups(self.count), threadsPerThreadgroup: LINE];
                set(&self.grid_update, &[&self.cells, &self.velocity]);
                let _: () = msg_send![&*e, dispatchThreadgroups: groups(cells), threadsPerThreadgroup: LINE];
                set(&self.g2p, &[&self.particles, &self.velocity]);
                let _: () = msg_send![&*e, dispatchThreadgroups: groups(self.count), threadsPerThreadgroup: LINE];
            }
            let _: () = msg_send![&*e, endEncoding];
        }
    }

    /// Where the drops are now, in grid cells. Only after the GPU is done with them.
    fn positions(&self) -> Vec<[f32; 3]> {
        // SAFETY: the buffer is shared and holds `count` particles of PARTICLE bytes.
        unsafe {
            let base: *mut c_void = msg_send![&*self.particles, contents];
            let base = base.cast::<f32>();
            (0..self.count).map(|i| *base.add(i * PARTICLE / 4).cast::<[f32; 3]>()).collect()
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Rendering

struct Targets {
    size: (usize, usize),
    depth: Id,
    smoothed: Id,
    thickness: Id,
    zbuffer: Id,
}

struct Renderer {
    depth: Id,
    thickness: Id,
    composite: Id,
    smooth: Id,
    depth_test: Id,
    targets: RefCell<Option<Targets>>,
}

impl Renderer {
    fn new(gpu: &Gpu) -> Result<Renderer, String> {
        // SAFETY: MTLDepthStencilDescriptor's documented setters.
        let depth_test: Option<Id> = unsafe {
            let d: Id = msg_send![class!(MTLDepthStencilDescriptor), new];
            let _: () = msg_send![&*d, setDepthCompareFunction: COMPARE_LESS];
            let _: () = msg_send![&*d, setDepthWriteEnabled: Bool::YES];
            msg_send![&*gpu.device, newDepthStencilStateWithDescriptor: &*d]
        };
        Ok(Renderer {
            depth: gpu.render("sprite_vs", "depth_fs", R32_FLOAT, true, false)?,
            thickness: gpu.render("sprite_vs", "thickness_fs", R16_FLOAT, false, true)?,
            composite: gpu.render("full_vs", "composite_fs", BGRA8_SRGB, false, false)?,
            smooth: gpu.compute("smooth")?,
            depth_test: depth_test.ok_or("no depth state")?,
            targets: RefCell::new(None),
        })
    }

    fn targets(&self, gpu: &Gpu, size: (usize, usize)) -> Result<(), String> {
        if self.targets.borrow().as_ref().is_some_and(|t| t.size == size) {
            return Ok(());
        }
        let (w, h) = size;
        *self.targets.borrow_mut() = Some(Targets {
            size,
            depth: gpu.texture(R32_FLOAT, w, h)?,
            smoothed: gpu.texture(R32_FLOAT, w, h)?,
            thickness: gpu.texture(R16_FLOAT, w, h)?,
            zbuffer: gpu.texture(DEPTH32_FLOAT, w, h)?,
        });
        Ok(())
    }

    /// Draws the water into `target`, a texture `size` pixels big.
    fn draw(
        &self,
        gpu: &Gpu,
        fluid: &Fluid,
        cmd: &AnyObject,
        target: &AnyObject,
        size: (usize, usize),
        view: &ViewParams,
    ) -> Result<(), String> {
        self.targets(gpu, size)?;
        let targets = self.targets.borrow();
        let t = targets.as_ref().expect("made above");
        let bytes = (view as *const ViewParams).cast::<c_void>();
        let len = size_of::<ViewParams>();
        // SAFETY: Metal's documented render and compute encoding; every object outlives the
        // command buffer, and the view parameters are copied when they are set.
        unsafe {
            let pass = |colour: &AnyObject, clear: Option<f64>, zbuffer: Option<&AnyObject>| -> Id {
                let d: Id = msg_send![class!(MTLRenderPassDescriptor), renderPassDescriptor];
                let attachments: Id = msg_send![&*d, colorAttachments];
                let a: Id = msg_send![&*attachments, objectAtIndexedSubscript: 0usize];
                let _: () = msg_send![&*a, setTexture: colour];
                let _: () = msg_send![&*a, setLoadAction: if clear.is_some() { LOAD_CLEAR } else { LOAD_DONT_CARE }];
                let _: () = msg_send![&*a, setStoreAction: STORE];
                let v = clear.unwrap_or(0.0);
                let _: () = msg_send![&*a, setClearColor: ClearColor { r: v, g: v, b: v, a: v }];
                if let Some(z) = zbuffer {
                    let da: Id = msg_send![&*d, depthAttachment];
                    let _: () = msg_send![&*da, setTexture: z];
                    let _: () = msg_send![&*da, setLoadAction: LOAD_CLEAR];
                    let _: () = msg_send![&*da, setClearDepth: 1.0f64];
                    let _: () = msg_send![&*da, setStoreAction: STORE_DONT_CARE];
                }
                msg_send![cmd, renderCommandEncoderWithDescriptor: &*d]
            };
            let sprites = |e: &Id, pipeline: &Id| {
                let _: () = msg_send![&**e, setRenderPipelineState: &**pipeline];
                let _: () = msg_send![&**e, setVertexBuffer: &*fluid.particles, offset: 0usize, atIndex: 0usize];
                let _: () = msg_send![&**e, setVertexBytes: bytes, length: len, atIndex: 1usize];
                let _: () = msg_send![&**e, setFragmentBytes: bytes, length: len, atIndex: 1usize];
                let _: () = msg_send![&**e, drawPrimitives: TRIANGLE_STRIP, vertexStart: 0usize, vertexCount: 4usize, instanceCount: fluid.count];
                let _: () = msg_send![&**e, endEncoding];
            };

            let e = pass(&t.depth, Some(0.0), Some(&t.zbuffer));
            let _: () = msg_send![&*e, setDepthStencilState: &*self.depth_test];
            sprites(&e, &self.depth);
            let e = pass(&t.thickness, Some(0.0), None);
            sprites(&e, &self.thickness);

            // Twice across and down, ending back in `depth`.
            let e: Id = msg_send![cmd, computeCommandEncoder];
            let _: () = msg_send![&*e, setComputePipelineState: &*self.smooth];
            let _: () = msg_send![&*e, setBytes: bytes, length: len, atIndex: 0usize];
            let tiles = MtlSize { width: size.0.div_ceil(16), height: size.1.div_ceil(16), depth: 1 };
            let tile = MtlSize { width: 16, height: 16, depth: 1 };
            for (dir, from, to) in [
                ([1i32, 0], &t.depth, &t.smoothed),
                ([0, 1], &t.smoothed, &t.depth),
                ([1, 0], &t.depth, &t.smoothed),
                ([0, 1], &t.smoothed, &t.depth),
            ] {
                let _: () = msg_send![&*e, setTexture: &**from, atIndex: 0usize];
                let _: () = msg_send![&*e, setTexture: &**to, atIndex: 1usize];
                let _: () = msg_send![&*e, setBytes: dir.as_ptr().cast::<c_void>(), length: 8usize, atIndex: 1usize];
                let _: () = msg_send![&*e, dispatchThreadgroups: tiles, threadsPerThreadgroup: tile];
            }
            let _: () = msg_send![&*e, endEncoding];

            let e = pass(target, None, None);
            let _: () = msg_send![&*e, setRenderPipelineState: &*self.composite];
            let _: () = msg_send![&*e, setFragmentTexture: &*t.depth, atIndex: 0usize];
            let _: () = msg_send![&*e, setFragmentTexture: &*t.thickness, atIndex: 1usize];
            let _: () = msg_send![&*e, setFragmentBytes: bytes, length: len, atIndex: 0usize];
            let _: () = msg_send![&*e, drawPrimitives: TRIANGLE, vertexStart: 0usize, vertexCount: 3usize];
            let _: () = msg_send![&*e, endEncoding];
        }
        Ok(())
    }
}

/// The camera, from how far round and how far up it is. `lift` is how much of the bottom of
/// the view is covered (by the field), so the ball sits in the middle of the rest.
fn view_params(yaw: f32, pitch: f32, size: (usize, usize), lift: f32, seconds: f32) -> ViewParams {
    let distance = 1.85;
    let eye = [yaw.sin() * pitch.cos() * distance, pitch.sin() * distance, yaw.cos() * pitch.cos() * distance];
    let view = look_at(eye, [0.0, -0.02, 0.0]);
    let proj = perspective(0.75, size.0 as f32 / size.1.max(1) as f32, lift, 0.05, 50.0);
    let identity = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]];
    let (w, h) = (size.0 as f32, size.1 as f32);
    let scale = 1.0 / 32.0;
    let radius = SPACING * scale * 1.6;
    // Drops overlap, so their thicknesses add up to more than the water there is.
    let overlap = (SPACING * scale).powi(3) / (4.0 / 3.0 * std::f32::consts::PI * radius.powi(3));
    let (r, g, b) = crate::WATER;
    let sun = normalize([-0.5, 0.75, 0.45]);
    ViewParams {
        view,
        proj,
        inv_view: inverse(&view).unwrap_or(identity),
        inv_proj: inverse(&proj).unwrap_or(identity),
        screen: [w, h, 1.0 / w, 1.0 / h],
        drop: [radius, scale, GRID as f32 / 2.0, radius * 3.0],
        sun: [sun[0], sun[1], sun[2], seconds],
        eye: [eye[0], eye[1], eye[2], 1.0],
        water: [r as f32, g as f32, b as f32, overlap],
    }
}

// ---------------------------------------------------------------------------------------------
// The self-test

/// Compiles the shaders and runs the water for a while on the GPU, then checks it held
/// together. On a Mac without Metal there is nothing to check.
pub fn selftest() -> Result<(), String> {
    let gpu = match Gpu::new() {
        Ok(g) => g,
        Err(e) if e.contains("no Metal device") || e.contains("isn't available") => {
            println!("selftest: {e}; the water wasn't checked");
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    let fluid = Fluid::new(&gpu)?;
    Renderer::new(&gpu)?;
    autoreleasepool(|_| {
        // SAFETY: a command buffer from the queue, waited for before its buffers are read.
        unsafe {
            let cmd: Id = msg_send![&*gpu.queue, commandBuffer];
            for frame in 0..90 {
                let impulse = if frame % 30 == 0 { [32.0, 32.0 + RADIUS, 32.0, 1.2] } else { [0.0; 4] };
                let (tide, swirl) = motion(frame as f32 / 60.0 * 8.0);
                fluid.encode(&cmd, &Forces { impulse, burst: 0.0, tide, swirl });
            }
            let _: () = msg_send![&*cmd, commit];
            let _: () = msg_send![&*cmd, waitUntilCompleted];
            let error: *mut AnyObject = msg_send![&*cmd, error];
            if !error.is_null() {
                return Err(format!("the GPU failed: {}", describe(error)));
            }
            Ok(())
        }
    })?;
    check(&fluid.positions())
}

/// The drops are numbers, inside the grid, and still a ball.
fn check(drops: &[[f32; 3]]) -> Result<(), String> {
    let c = GRID as f32 / 2.0;
    if drops.iter().flatten().any(|v| !v.is_finite()) {
        return Err("the water blew up".into());
    }
    let mean =
        drops.iter().map(|p| dot(sub(*p, [c, c, c]), sub(*p, [c, c, c])).sqrt()).sum::<f32>() / drops.len() as f32;
    // At rest the drops' mean distance from the centre is three quarters of the radius.
    if !(RADIUS * 0.4..RADIUS * 1.4).contains(&mean) {
        return Err(format!("the water lost its shape (mean distance {mean:.1}, radius {RADIUS})"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The view

extern_class!(
    // SAFETY: an NSObject subclass, only used as a superclass.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSView"]
    pub struct NSView;
);

struct Scene {
    gpu: Gpu,
    fluid: Fluid,
    renderer: Renderer,
    layer: Id,
}

pub struct State {
    scene: Option<Scene>,
    /// The length of the text last typed, to tell typing from sending.
    typed: Cell<usize>,
    yaw: Cell<f32>,
    pitch: Cell<f32>,
    impulse: Cell<[f32; 4]>,
    burst: Cell<f32>,
    seed: Cell<u32>,
    start: std::time::Instant,
    /// When the water ripples by itself next, in seconds since `start`.
    next_ripple: Cell<f32>,
    failed: Cell<bool>,
    /// The height at the bottom that something covers, in points.
    covered: f64,
}

define_class!(
    // SAFETY: an NSView hosting a CAMetalLayer that it draws the water into; it is its timer's
    // target. It adds no Drop.
    #[unsafe(super(NSView, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AppWater"]
    #[ivars = State]
    pub struct WaterView;

    impl WaterView {
        #[unsafe(method(tick:))]
        fn tick(&self, _timer: *mut AnyObject) {
            autoreleasepool(|_| self.frame());
        }

        /// Any text in the app changed: a key was typed, or the field was sent and emptied.
        #[unsafe(method(textChanged:))]
        fn text_changed(&self, note: *mut AnyObject) {
            // SAFETY: NSNotification and NSText messages on the main thread.
            let len: usize = unsafe {
                let text: *mut AnyObject = msg_send![note, object];
                if text.is_null() {
                    return;
                }
                let s: Id = msg_send![text, string];
                msg_send![&*s, length]
            };
            let before = self.ivars().typed.replace(len);
            if len == 0 && before > 0 { self.ivars().burst.set(0.8) } else { self.ripple(1.2) }
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> Bool {
            // With Faust, the field takes the keys; on its own, the water does.
            Bool::new(faust_link::get().is_none())
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, _event: *mut AnyObject) {
            self.ripple(1.2);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: *mut AnyObject) {
            // SAFETY: -[NSEvent deltaX] and -deltaY return CGFloats.
            let (dx, dy): (f64, f64) = unsafe { (msg_send![event, deltaX], msg_send![event, deltaY]) };
            let s = self.ivars();
            s.yaw.set(s.yaw.get() - dx as f32 * 0.008);
            s.pitch.set((s.pitch.get() + dy as f32 * 0.006).clamp(-0.1, 1.2));
        }
    }
);

impl WaterView {
    /// A view `frame` big, already moving and listening for typing, with the ball centred above
    /// the bottom `covered` points. Without Metal it stays empty, and the app works all the same.
    pub fn new(mtm: MainThreadMarker, frame: Rect, covered: f64) -> Retained<WaterView> {
        let scene = Gpu::new().and_then(|gpu| {
            let fluid = Fluid::new(&gpu)?;
            let renderer = Renderer::new(&gpu)?;
            // SAFETY: CAMetalLayer's documented setters on the main thread.
            let layer: Id = unsafe {
                let layer: Id = msg_send![class!(CAMetalLayer), new];
                let _: () = msg_send![&*layer, setDevice: &*gpu.device];
                let _: () = msg_send![&*layer, setPixelFormat: BGRA8_SRGB];
                let _: () = msg_send![&*layer, setFramebufferOnly: Bool::YES];
                layer
            };
            Ok(Scene { gpu, fluid, renderer, layer })
        });
        let scene = scene.map_err(|e| eprintln!("the water is off: {e}")).ok();
        let state = State {
            scene,
            typed: Cell::new(0),
            yaw: Cell::new(0.4),
            pitch: Cell::new(0.32),
            impulse: Cell::new([0.0; 4]),
            burst: Cell::new(0.0),
            seed: Cell::new(0x9e37_79b9),
            start: std::time::Instant::now(),
            next_ripple: Cell::new(1.0),
            failed: Cell::new(false),
            covered,
        };
        let this = WaterView::alloc(mtm).set_ivars(state);
        // SAFETY: NSView, NSTimer and NSNotificationCenter messages on the main thread. The
        // timer and the centre hold the view for as long as the app runs.
        unsafe {
            let this: Retained<WaterView> = msg_send![super(this), initWithFrame: frame];
            if let Some(s) = &this.ivars().scene {
                let _: () = msg_send![&*this, setLayer: &*s.layer];
                let _: () = msg_send![&*this, setWantsLayer: Bool::YES];
            }
            let _: Id = msg_send![class!(NSTimer), scheduledTimerWithTimeInterval: 1.0f64 / 60.0, target: &*this, selector: sel!(tick:), userInfo: null::<AnyObject>(), repeats: Bool::YES];
            let centre: Id = msg_send![class!(NSNotificationCenter), defaultCenter];
            let _: () = msg_send![&*centre, addObserver: &*this, selector: sel!(textChanged:), name: &*nsstring("NSTextDidChangeNotification"), object: null::<AnyObject>()];
            this
        }
    }

    /// A ripple from a random spot on the side facing the camera, `strength` hard (a key is 1.2).
    fn ripple(&self, strength: f32) {
        let s = self.ivars();
        // A small xorshift keeps the spots varied without a dependency.
        let mut x = s.seed.get();
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            f64::from(x) as f32 / u32::MAX as f32 * 2.0 - 1.0
        };
        let (yaw, pitch) = (s.yaw.get(), s.pitch.get());
        let towards = [yaw.sin() * pitch.cos(), pitch.sin(), yaw.cos() * pitch.cos()];
        let d = normalize([towards[0] + next() * 0.9, towards[1] + next() * 0.9, towards[2] + next() * 0.9]);
        s.seed.set(x);
        let c = GRID as f32 / 2.0;
        s.impulse.set([c + d[0] * RADIUS, c + d[1] * RADIUS, c + d[2] * RADIUS, strength]);
    }

    fn frame(&self) {
        let s = self.ivars();
        let Some(scene) = &s.scene else { return };
        if s.failed.get() {
            return;
        }
        s.yaw.set(s.yaw.get() + 0.0015);
        // SAFETY: NSView, CAMetalLayer and Metal command buffer messages on the main thread.
        unsafe {
            let bounds: Rect = msg_send![self, bounds];
            let window: Option<Id> = msg_send![self, window];
            let backing: f64 = window.as_ref().map_or(2.0, |w| msg_send![&**w, backingScaleFactor]);
            let size = ((bounds.w * backing) as usize, (bounds.h * backing) as usize);
            if size.0 < 8 || size.1 < 8 {
                return;
            }
            let _: () = msg_send![&*scene.layer, setContentsScale: backing];
            let _: () = msg_send![&*scene.layer, setDrawableSize: Size { w: size.0 as f64, h: size.1 as f64 }];
            let drawable: Option<Id> = msg_send![&*scene.layer, nextDrawable];
            let Some(drawable) = drawable else { return };
            let texture: Id = msg_send![&*drawable, texture];
            let t = s.start.elapsed().as_secs_f32();
            if t >= s.next_ripple.get() {
                self.ripple(0.7);
                // Every second or so, not quite regularly.
                let jitter = (s.seed.get() % 1000) as f32 / 1000.0;
                s.next_ripple.set(t + 0.7 + jitter * 0.8);
            }
            let (tide, swirl) = motion(t);
            let cmd: Id = msg_send![&*scene.gpu.queue, commandBuffer];
            let forces = Forces { impulse: s.impulse.get(), burst: s.burst.get(), tide, swirl };
            scene.fluid.encode(&cmd, &forces);
            let mut impulse = s.impulse.get();
            impulse[3] *= 0.6;
            s.impulse.set(impulse);
            s.burst.set(s.burst.get() * 0.8);
            let lift = (s.covered / bounds.h.max(1.0)) as f32;
            let view = view_params(s.yaw.get(), s.pitch.get(), size, lift, s.start.elapsed().as_secs_f32());
            if let Err(e) = scene.renderer.draw(&scene.gpu, &scene.fluid, &cmd, &texture, size, &view) {
                eprintln!("the water stopped: {e}");
                s.failed.set(true);
                return;
            }
            let _: () = msg_send![&*cmd, presentDrawable: &*drawable];
            let _: () = msg_send![&*cmd, commit];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shader_structs_have_metals_sizes() {
        assert_eq!(size_of::<SimParams>(), 112);
        assert_eq!(size_of::<ViewParams>(), 4 * 64 + 5 * 16);
    }

    #[test]
    fn the_ball_starts_inside_the_grid() {
        let drops = starting_drops();
        assert!(drops.len() > 50_000, "{} drops", drops.len());
        assert!(drops.iter().flatten().all(|v| (2.0..GRID as f32 - 3.0).contains(v)));
        assert_eq!(check(&drops), Ok(()));
    }

    #[test]
    fn inverses_undo() {
        let v = view_params(0.4, 0.3, (800, 600), 0.2, 0.0);
        for (m, inv) in [(v.view, v.inv_view), (v.proj, v.inv_proj)] {
            let id = mul(&m, &inv);
            for (c, col) in id.iter().enumerate() {
                for (r, x) in col.iter().enumerate() {
                    let want = if c == r { 1.0 } else { 0.0 };
                    assert!((x - want).abs() < 1e-4, "{id:?}");
                }
            }
        }
    }

    #[test]
    fn runs_on_the_gpu() {
        assert_eq!(selftest(), Ok(()));
    }
}
