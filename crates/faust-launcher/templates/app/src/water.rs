//! A ball of water that ripples as the person types. A few hundred drops are pulled toward the
//! centre and push each other apart when they crowd, which is enough to look like a liquid;
//! the view draws them as overlapping translucent circles, lighter where they move fast.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, class, define_class, extern_class, msg_send, sel};

use crate::{Id, Rect, nsstring};

const DROPS: usize = 260;
/// How far two drops feel each other, in ball radii.
const REACH: f64 = 0.16;
/// The pull toward the centre.
const GRAVITY: f64 = 2.0;
/// How hard crowded drops push apart, and how crowded they like to be. At rest the ball is
/// then about 0.8 across from its centre.
const STIFFNESS: f64 = 100.0;
const NEAR_STIFFNESS: f64 = 200.0;
const REST: f64 = 1.0;
const STEP: f64 = 1.0 / 120.0;

/// The drops, in a space where the ball is about 1 across from its centre.
pub struct Water {
    pos: Vec<[f64; 2]>,
    vel: Vec<[f64; 2]>,
    seed: Cell<u32>,
}

impl Water {
    /// Drops laid out in a sunflower spiral, at rest.
    pub fn new() -> Water {
        let golden = std::f64::consts::PI * (3.0 - 5f64.sqrt());
        let pos = (0..DROPS)
            .map(|i| {
                let r = 0.9 * ((i as f64 + 0.5) / DROPS as f64).sqrt();
                let a = i as f64 * golden;
                [r * a.cos(), r * a.sin()]
            })
            .collect();
        Water { pos, vel: vec![[0.0; 2]; DROPS], seed: Cell::new(7) }
    }

    /// Moves time on by one step.
    pub fn step(&mut self) {
        let prev = self.pos.clone();
        for (p, v) in self.pos.iter_mut().zip(&mut self.vel) {
            let d = (p[0] * p[0] + p[1] * p[1]).sqrt().max(1e-6);
            v[0] = (v[0] - GRAVITY * p[0] / d * STEP) * 0.998;
            v[1] = (v[1] - GRAVITY * p[1] / d * STEP) * 0.998;
            p[0] += v[0] * STEP;
            p[1] += v[1] * STEP;
        }
        self.relax();
        for ((p, v), q) in self.pos.iter_mut().zip(&mut self.vel).zip(&prev) {
            // Nothing flies off for good.
            let d = (p[0] * p[0] + p[1] * p[1]).sqrt();
            if d > 1.8 {
                p[0] *= 1.8 / d;
                p[1] *= 1.8 / d;
            }
            *v = [(p[0] - q[0]) / STEP, (p[1] - q[1]) / STEP];
        }
    }

    /// Pushes crowded drops apart, by how crowded each one is.
    fn relax(&mut self) {
        let n = self.pos.len();
        for i in 0..n {
            let (mut density, mut near) = (0.0, 0.0);
            for j in 0..n {
                if let Some(q) = self.closeness(i, j) {
                    density += q * q;
                    near += q * q * q;
                }
            }
            let pressure = STIFFNESS * (density - REST);
            let near_pressure = NEAR_STIFFNESS * near;
            let mut moved = [0.0, 0.0];
            for j in 0..n {
                let Some(q) = self.closeness(i, j) else { continue };
                let (a, b) = (self.pos[i], self.pos[j]);
                let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                let d = (dx * dx + dy * dy).sqrt().max(1e-6);
                let push = STEP * STEP * (pressure * q + near_pressure * q * q) / 2.0;
                let (px, py) = (push * dx / d, push * dy / d);
                self.pos[j][0] += px;
                self.pos[j][1] += py;
                moved[0] -= px;
                moved[1] -= py;
            }
            self.pos[i][0] += moved[0];
            self.pos[i][1] += moved[1];
        }
    }

    /// 1 when drops `i` and `j` touch, falling to 0 at `REACH`; `None` beyond it.
    fn closeness(&self, i: usize, j: usize) -> Option<f64> {
        if i == j {
            return None;
        }
        let (a, b) = (self.pos[i], self.pos[j]);
        let d = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
        (d < REACH).then(|| 1.0 - d / REACH)
    }

    /// A ripple: drops facing a random direction are thrown outward. A key press is 0.5.
    pub fn splash(&mut self, strength: f64) {
        // A small xorshift keeps the directions varied without a dependency.
        let mut s = self.seed.get();
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        self.seed.set(s);
        let angle = f64::from(s) / f64::from(u32::MAX) * std::f64::consts::TAU;
        self.throw(angle, strength, 6);
    }

    /// Every drop is thrown outward: the request went off.
    pub fn burst(&mut self) {
        self.throw(0.0, 1.2, 0);
    }

    /// Throws drops outward, the most those facing `angle` (`focus` 0: all alike).
    fn throw(&mut self, angle: f64, strength: f64, focus: i32) {
        let (cx, cy) = (angle.cos(), angle.sin());
        for (p, v) in self.pos.iter().zip(&mut self.vel) {
            let d = (p[0] * p[0] + p[1] * p[1]).sqrt().max(1e-6);
            let (ux, uy) = (p[0] / d, p[1] / d);
            let facing = ((ux * cx + uy * cy).max(0.0)).powi(focus);
            let kick = strength * 3.0 * facing * d;
            v[0] += kick * ux;
            v[1] += kick * uy;
        }
    }

    /// How far the furthest drop is from the centre.
    pub fn reach(&self) -> f64 {
        self.pos.iter().map(|p| (p[0] * p[0] + p[1] * p[1]).sqrt()).fold(0.0, f64::max)
    }

    /// Everything is still a number: nothing blew up.
    pub fn is_sound(&self) -> bool {
        self.pos.iter().chain(&self.vel).all(|p| p[0].is_finite() && p[1].is_finite())
    }
}

/// Runs the water for a while, splashing it, and checks it holds together.
pub fn selftest() -> Result<(), String> {
    let mut w = Water::new();
    for i in 0..360 {
        if i % 40 == 0 {
            w.splash(1.0);
        }
        w.step();
    }
    if !w.is_sound() {
        return Err("the water blew up".into());
    }
    if w.reach() > 1.8 {
        return Err(format!("the water came apart (reach {:.2})", w.reach()));
    }
    Ok(())
}

extern_class!(
    // SAFETY: an NSObject subclass, only used as a superclass.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NSView"]
    pub struct NSView;
);

pub struct State {
    water: RefCell<Water>,
    /// The length of the text last typed, to tell typing from sending.
    typed: Cell<usize>,
}

define_class!(
    // SAFETY: an NSView that draws the water and is its timer's target. It adds no Drop.
    #[unsafe(super(NSView, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AppWater"]
    #[ivars = State]
    pub struct WaterView;

    impl WaterView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: Rect) {
            self.draw();
        }

        #[unsafe(method(tick:))]
        fn tick(&self, _timer: *mut AnyObject) {
            {
                let mut w = self.ivars().water.borrow_mut();
                w.step();
                w.step();
            }
            // SAFETY: -[NSView setNeedsDisplay:] on the main thread.
            let _: () = unsafe { msg_send![self, setNeedsDisplay: Bool::YES] };
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
            let mut w = self.ivars().water.borrow_mut();
            if len == 0 && before > 0 { w.burst() } else { w.splash(0.5) }
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> Bool {
            Bool::YES
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, _event: *mut AnyObject) {
            self.ivars().water.borrow_mut().splash(0.5);
        }
    }
);

impl WaterView {
    /// A view `frame` big, already moving and listening for typing.
    pub fn new(mtm: MainThreadMarker, frame: Rect) -> Retained<WaterView> {
        let this = WaterView::alloc(mtm).set_ivars(State { water: RefCell::new(Water::new()), typed: Cell::new(0) });
        // SAFETY: NSView, NSTimer and NSNotificationCenter messages on the main thread. The
        // timer and the centre hold the view for as long as the app runs.
        unsafe {
            let this: Retained<WaterView> = msg_send![super(this), initWithFrame: frame];
            let _: Id = msg_send![class!(NSTimer), scheduledTimerWithTimeInterval: 1.0f64 / 60.0, target: &*this, selector: sel!(tick:), userInfo: std::ptr::null::<AnyObject>(), repeats: Bool::YES];
            let centre: Id = msg_send![class!(NSNotificationCenter), defaultCenter];
            let _: () = msg_send![&*centre, addObserver: &*this, selector: sel!(textChanged:), name: &*nsstring("NSTextDidChangeNotification"), object: std::ptr::null::<AnyObject>()];
            this
        }
    }

    fn draw(&self) {
        let w = self.ivars().water.borrow();
        let (red, green, blue) = crate::WATER;
        // SAFETY: NSView, NSColor and NSBezierPath messages while the view draws.
        unsafe {
            let b: Rect = msg_send![self, bounds];
            let scale = b.w.min(b.h) * 0.46;
            let (cx, cy) = (b.w / 2.0, b.h / 2.0);
            let oval = |x: f64, y: f64, r: f64, color: &Id| {
                let path: Id = msg_send![class!(NSBezierPath), bezierPathWithOvalInRect: Rect { x: x - r, y: y - r, w: r * 2.0, h: r * 2.0 }];
                let _: () = msg_send![&**color, set];
                let _: () = msg_send![&*path, fill];
            };
            let rgba = |r: f64, g: f64, b: f64, a: f64| -> Id {
                msg_send![class!(NSColor), colorWithSRGBRed: r, green: g, blue: b, alpha: a]
            };
            let lit =
                |light: f64| (red + (1.0 - red) * light, green + (1.0 - green) * light, blue + (1.0 - blue) * light);
            // A soft edge first, then the body, so the drops run together.
            for (radius, alpha) in [(0.13, 0.12), (0.09, 1.0)] {
                for (p, v) in w.pos.iter().zip(&w.vel) {
                    // Fast drops catch the light.
                    let (r, g, b) = lit(((v[0] * v[0] + v[1] * v[1]).sqrt() / 2.0).min(1.0) * 0.7);
                    oval(cx + p[0] * scale, cy + p[1] * scale, scale * radius, &rgba(r, g, b, alpha));
                }
            }
            // A highlight, up and to the left, as on a drop.
            oval(cx - scale * 0.28, cy + scale * 0.32, scale * 0.12, &rgba(1.0, 1.0, 1.0, 0.18));
            oval(cx - scale * 0.32, cy + scale * 0.37, scale * 0.05, &rgba(1.0, 1.0, 1.0, 0.45));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holds_together() {
        assert_eq!(selftest(), Ok(()));
    }

    #[test]
    fn a_splash_throws_drops_out() {
        let mut w = Water::new();
        for _ in 0..240 {
            w.step();
        }
        let calm = w.reach();
        w.burst();
        for _ in 0..10 {
            w.step();
        }
        assert!(w.reach() > calm, "{} should be beyond {calm}", w.reach());
    }
}
