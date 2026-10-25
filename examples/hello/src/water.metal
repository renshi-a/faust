// The water's shaders, compiled by Metal when the app starts (no Xcode needed).
//
// Simulation: the material point method (MLS-MPM) on a 64³ grid. Each step, the drops hand
// their mass and momentum to the grid (p2g_*), the grid moves them on (grid_update), and the
// drops take their new velocity and its gradient back (g2p). At the centre of the grid is a
// solid ball; the water is pulled toward it and runs over it like a sea over a small planet.
//
// Rendering: every drop is a small light, drawn out along its motion and brighter the faster
// it goes, added up in a high-range texture (glow). A blurred copy of that makes it bloom
// (bloom_*), and composite puts both over a dark backdrop and the planet. The drops are also
// drawn as little spheres into a depth texture, so the pointer can find the water under it.

#include <metal_stdlib>
using namespace metal;

struct Particle {
    float3 x;   // position, in grid cells
    float3 v;   // velocity
    float3x3 c; // affine velocity field around it
};

// The grid's sums, as fixed-point integers so that many drops can add to a cell at once.
struct Cell {
    atomic_int vx;
    atomic_int vy;
    atomic_int vz;
    atomic_int m;
};

struct Sim {
    float4 centre;  // xyz: the planet's centre; w: the pull toward it
    float4 impulse; // xyz: where a ripple starts; w: how hard
    float4 fluid;   // x: time step, y: rest density, z: stiffness, w: viscosity
    float4 misc;    // x: fixed-point scale, y: outward burst, z: ripple size, w: the planet's radius
    float4 swirl;   // xyz: the axis the water turns about, w: how fast (radians per time)
    float4 tide;    // xyz: the axis the water is drawn out along, w: how hard
    float4 mouse;   // xy: the pointer, as a fraction of the view; z: its reach in cells; w: on
    float4 drag;    // xyz: the pointer's velocity in the grid, in cells per time
    uint4 size;     // x: drops, y: grid side
};

struct View {
    float4x4 view;
    float4x4 proj;
    float4x4 inv_view;
    float4x4 inv_proj;
    float4 screen; // width, height, 1 / width, 1 / height (pixels)
    float4 drop;   // x: drawn radius, y: grid-to-world scale, z: grid centre, w: the planet's radius
    float4 sun;    // xyz: toward the light
    float4 eye;    // xyz: the camera
    float4 water;  // rgb: the water's colour (linear), w: how much light one drop gives
};

// A point in view space, from where it is on screen (0 to 1, y down) and how far away.
static float3 view_pos(float2 uv, float d, constant View &u) {
    float2 ndc = float2(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    float4 r = u.inv_proj * float4(ndc, 1.0, 1.0);
    float3 ray = r.xyz / r.w;
    return ray * (d / -ray.z);
}

static uint cell_at(int3 c, int side) {
    return uint((c.z * side + c.y) * side + c.x);
}

// Quadratic B-spline weights for the 3x3x3 cells around `x`.
static int3 weights(float3 x, thread float3 w[3]) {
    int3 cell = int3(floor(x));
    float3 d = x - float3(cell) - 0.5;
    w[0] = 0.5 * (0.5 - d) * (0.5 - d);
    w[1] = 0.75 - d * d;
    w[2] = 0.5 * (0.5 + d) * (0.5 + d);
    return cell;
}

static int fixed(float v, float scale) {
    return int(clamp(v * scale, -2.0e9, 2.0e9));
}

kernel void clear_grid(device Cell *cells [[buffer(0)]],
                       constant Sim &s [[buffer(1)]],
                       uint id [[thread_position_in_grid]]) {
    if (id >= s.size.y * s.size.y * s.size.y) return;
    atomic_store_explicit(&cells[id].vx, 0, memory_order_relaxed);
    atomic_store_explicit(&cells[id].vy, 0, memory_order_relaxed);
    atomic_store_explicit(&cells[id].vz, 0, memory_order_relaxed);
    atomic_store_explicit(&cells[id].m, 0, memory_order_relaxed);
}

// Mass and momentum, with the affine part so that rotation survives the trip.
kernel void p2g_mass(device const Particle *p [[buffer(0)]],
                     device Cell *cells [[buffer(1)]],
                     constant Sim &s [[buffer(2)]],
                     uint id [[thread_position_in_grid]]) {
    if (id >= s.size.x) return;
    Particle q = p[id];
    float3 w[3];
    int3 base = weights(q.x, w);
    int side = int(s.size.y);
    float scale = s.misc.x;
    for (int i = 0; i < 3; i++)
    for (int j = 0; j < 3; j++)
    for (int k = 0; k < 3; k++) {
        int3 n = base + int3(i, j, k) - 1;
        float3 dist = float3(n) - q.x + 0.5;
        float weight = w[i].x * w[j].y * w[k].z;
        float3 momentum = weight * (q.v + q.c * dist);
        uint c = cell_at(n, side);
        atomic_fetch_add_explicit(&cells[c].vx, fixed(momentum.x, scale), memory_order_relaxed);
        atomic_fetch_add_explicit(&cells[c].vy, fixed(momentum.y, scale), memory_order_relaxed);
        atomic_fetch_add_explicit(&cells[c].vz, fixed(momentum.z, scale), memory_order_relaxed);
        atomic_fetch_add_explicit(&cells[c].m, fixed(weight, scale), memory_order_relaxed);
    }
}

// Pressure and viscosity: each drop reads how dense the grid is around it and pushes back.
kernel void p2g_stress(device const Particle *p [[buffer(0)]],
                       device Cell *cells [[buffer(1)]],
                       constant Sim &s [[buffer(2)]],
                       uint id [[thread_position_in_grid]]) {
    if (id >= s.size.x) return;
    Particle q = p[id];
    float3 w[3];
    int3 base = weights(q.x, w);
    int side = int(s.size.y);
    float scale = s.misc.x;
    float density = 0.0;
    for (int i = 0; i < 3; i++)
    for (int j = 0; j < 3; j++)
    for (int k = 0; k < 3; k++) {
        int3 n = base + int3(i, j, k) - 1;
        float m = float(atomic_load_explicit(&cells[cell_at(n, side)].m, memory_order_relaxed)) / scale;
        density += w[i].x * w[j].y * w[k].z * m;
    }
    float dt = s.fluid.x;
    float volume = 1.0 / max(density, 1e-3);
    float pressure = clamp(s.fluid.z * (pow(density / s.fluid.y, 5.0) - 1.0), 0.0, 200.0);
    float3x3 strain = q.c + transpose(q.c);
    float3x3 stress = float3x3(-pressure) + s.fluid.w * strain;
    float3x3 term = -volume * 4.0 * dt * stress;
    for (int i = 0; i < 3; i++)
    for (int j = 0; j < 3; j++)
    for (int k = 0; k < 3; k++) {
        int3 n = base + int3(i, j, k) - 1;
        float3 dist = float3(n) - q.x + 0.5;
        float3 momentum = w[i].x * w[j].y * w[k].z * (term * dist);
        uint c = cell_at(n, side);
        atomic_fetch_add_explicit(&cells[c].vx, fixed(momentum.x, scale), memory_order_relaxed);
        atomic_fetch_add_explicit(&cells[c].vy, fixed(momentum.y, scale), memory_order_relaxed);
        atomic_fetch_add_explicit(&cells[c].vz, fixed(momentum.z, scale), memory_order_relaxed);
    }
}

// Velocities from momentum, then everything that moves the water besides itself: the planet's
// pull and its surface, the tide and the turn, ripples, the pointer, and the grid's walls.
kernel void grid_update(device Cell *cells [[buffer(0)]],
                        device float4 *velocity [[buffer(1)]],
                        constant Sim &s [[buffer(2)]],
                        constant View &u [[buffer(3)]],
                        texture2d<float, access::read> depth [[texture(0)]],
                        uint id [[thread_position_in_grid]]) {
    int side = int(s.size.y);
    if (id >= uint(side * side * side)) return;
    float scale = s.misc.x;
    float m = float(atomic_load_explicit(&cells[id].m, memory_order_relaxed)) / scale;
    if (m <= 1e-5) {
        velocity[id] = float4(0.0);
        return;
    }
    float3 v = float3(float(atomic_load_explicit(&cells[id].vx, memory_order_relaxed)),
                      float(atomic_load_explicit(&cells[id].vy, memory_order_relaxed)),
                      float(atomic_load_explicit(&cells[id].vz, memory_order_relaxed))) / scale / m;
    int3 c = int3(id % uint(side), (id / uint(side)) % uint(side), id / uint(side * side));
    float3 pos = float3(c) + 0.5;
    float dt = s.fluid.x;
    float3 out = pos - s.centre.xyz;
    float r = length(out);
    float3 outward = r > 1e-4 ? out / r : float3(0.0, 1.0, 0.0);

    v -= outward * s.centre.w * dt;
    v += s.tide.w * (3.0 * dot(out, s.tide.xyz) * s.tide.xyz - out) * dt;
    // The water is eased toward a slow turn, which also keeps it from spinning up.
    v += (cross(s.swirl.xyz, out) * s.swirl.w - v) * 0.01 * dt;
    if (s.impulse.w > 0.0) {
        float3 d = pos - s.impulse.xyz;
        float size = s.misc.z;
        v += outward * s.impulse.w * exp(-dot(d, d) / (size * size)) * dt;
    }
    v += outward * s.misc.y * dt;

    // The pointer: where it is over the water, read from last frame's depth, and the water
    // near there is drawn along with it.
    if (s.mouse.w > 0.0) {
        uint2 px = uint2(clamp(s.mouse.xy, 0.0, 0.999) * float2(depth.get_width(), depth.get_height()));
        float d = depth.read(px).r;
        if (d > 0.0) {
            float3 world = (u.inv_view * float4(view_pos(s.mouse.xy, d, u), 1.0)).xyz;
            float3 at = world / u.drop.y + u.drop.z;
            float3 to = pos - at;
            float reach = s.mouse.z;
            float near = saturate(1.0 - dot(to, to) / (reach * reach));
            v = mix(v, v + s.drag.xyz, near * 0.3);
        }
    }

    // The planet's surface: water slides over it but doesn't go in.
    if (r < s.misc.w + 1.0) {
        float into = dot(v, outward);
        if (into < 0.0) v -= outward * into;
    }
    // The grid's walls: nothing leaves through them.
    if (c.x < 2 && v.x < 0.0) v.x = 0.0;
    if (c.y < 2 && v.y < 0.0) v.y = 0.0;
    if (c.z < 2 && v.z < 0.0) v.z = 0.0;
    if (c.x > side - 3 && v.x > 0.0) v.x = 0.0;
    if (c.y > side - 3 && v.y > 0.0) v.y = 0.0;
    if (c.z > side - 3 && v.z > 0.0) v.z = 0.0;
    velocity[id] = float4(v, m);
}

kernel void g2p(device Particle *p [[buffer(0)]],
                device const float4 *velocity [[buffer(1)]],
                constant Sim &s [[buffer(2)]],
                uint id [[thread_position_in_grid]]) {
    if (id >= s.size.x) return;
    Particle q = p[id];
    float3 w[3];
    int3 base = weights(q.x, w);
    int side = int(s.size.y);
    float3 v = float3(0.0);
    float3x3 b = float3x3(0.0);
    for (int i = 0; i < 3; i++)
    for (int j = 0; j < 3; j++)
    for (int k = 0; k < 3; k++) {
        int3 n = base + int3(i, j, k) - 1;
        float3 dist = float3(n) - q.x + 0.5;
        float weight = w[i].x * w[j].y * w[k].z;
        float3 gv = velocity[cell_at(n, side)].xyz;
        v += weight * gv;
        b += weight * float3x3(gv * dist.x, gv * dist.y, gv * dist.z);
    }
    q.v = v;
    q.c = b * 4.0;
    float3 x = clamp(q.x + v * s.fluid.x, float3(2.0), float3(float(side) - 3.0));
    // Nothing ends up inside the planet.
    float3 out = x - s.centre.xyz;
    float r = length(out);
    if (r < s.misc.w && r > 1e-4) x = s.centre.xyz + out / r * s.misc.w;
    q.x = x;
    p[id] = q;
}

// ---------------------------------------------------------------------------------------------
// Rendering

struct Sprite {
    float4 pos [[position]];
    float2 uv;
    float3 centre; // in view space
};

vertex Sprite sprite_vs(uint vid [[vertex_id]],
                        uint iid [[instance_id]],
                        device const Particle *p [[buffer(0)]],
                        constant View &u [[buffer(1)]]) {
    float3 world = (p[iid].x - u.drop.z) * u.drop.y;
    float3 c = (u.view * float4(world, 1.0)).xyz;
    float2 uv = float2(float(vid & 1) * 2.0 - 1.0, float(vid >> 1) * 2.0 - 1.0);
    Sprite s;
    s.uv = uv;
    s.centre = c;
    s.pos = u.proj * float4(c + float3(uv * u.drop.x, 0.0), 1.0);
    return s;
}

struct DepthOut {
    float depth [[color(0)]];
    float z [[depth(any)]];
};

// The front of each drop's sphere: its distance from the camera, and the depth to test. Only
// the pointer uses it, to find the water under it.
fragment DepthOut depth_fs(Sprite s [[stage_in]], constant View &u [[buffer(1)]]) {
    float r2 = dot(s.uv, s.uv);
    if (r2 > 1.0) discard_fragment();
    float3 p = s.centre + float3(s.uv, sqrt(1.0 - r2)) * u.drop.x;
    float4 clip = u.proj * float4(p, 1.0);
    DepthOut o;
    o.depth = -p.z;
    o.z = clip.z / clip.w;
    return o;
}

// The colour turned `a` radians around the grey axis: the same water, a little more teal one
// way and a little more violet the other.
static float3 hue(float3 c, float a) {
    float3 k = float3(0.57735);
    float cs = cos(a);
    return max(c * cs + cross(k, c) * sin(a) + k * dot(k, c) * (1.0 - cs), 0.0);
}

struct Glow {
    float4 pos [[position]];
    float2 uv;
    float3 light;
};

// Each drop as a small light, drawn out a little along the way it moves on screen. It is lit
// by the sun and brightest at the planet's edge, and drops that a wave lifts high glow white,
// like foam. Its colour drifts slowly around the water's own, in broad patches over the planet.
// Drops on the far side of the planet barely show through it.
vertex Glow glow_vs(uint vid [[vertex_id]],
                    uint iid [[instance_id]],
                    device const Particle *p [[buffer(0)]],
                    constant View &u [[buffer(1)]]) {
    Particle q = p[iid];
    float3 world = (q.x - u.drop.z) * u.drop.y;
    float3 c = (u.view * float4(world, 1.0)).xyz;
    float r = u.drop.x * 0.4;
    // How far it moves in a frame, on screen.
    float2 moving = (u.view * float4(q.v * u.drop.y, 0.0)).xy * 0.5;
    float trail = min(length(moving), r * 3.0);
    float2 along = trail > 1e-6 ? normalize(moving) : float2(1.0, 0.0);
    float2 across = float2(-along.y, along.x);
    float2 uv = float2(float(vid & 1) * 2.0 - 1.0, float(vid >> 1) * 2.0 - 1.0);
    // The quad reaches back along the trail, so the drop leads it.
    float2 offset = along * (uv.x < 0.0 ? -(r + trail) : r) + across * uv.y * r;

    float3 eye = u.eye.xyz;
    float3 to = world - eye;
    float far = length(to);
    float3 d = to / far;
    float planet = u.drop.w;
    float b = dot(eye, d);
    float h = b * b - (dot(eye, eye) - planet * planet);
    bool hidden = h > 0.0 && -b - sqrt(h) < far - r;
    float near = length(eye) - planet;
    float fade = mix(1.0, 0.4, saturate((far - near) / (2.0 * planet)));

    float3 up = normalize(world);
    float lit = 0.15 + 1.1 * saturate(dot(up, u.sun.xyz));
    float rim = pow(1.0 - abs(dot(up, d)), 2.0);
    // How high above the planet, in cells: the water lies about two deep.
    float height = length(world) / u.drop.y - planet / u.drop.y;
    float foam = smoothstep(2.0, 3.2, height);
    float fast = smoothstep(0.3, 2.5, length(q.v));
    float t = u.sun.w;
    float drift = sin(dot(up, float3(2.1, 1.3, -1.7)) + t * 0.07)
                + 0.6 * sin(dot(up, float3(-1.1, 2.6, 0.9)) * 1.7 - t * 0.05);
    float3 tint = hue(u.water.rgb, 0.2 * drift - 0.25 * rim);
    float3 colour = mix(tint, mix(tint, float3(1.0), 0.7), max(foam, fast * 0.6));
    // Not every drop is as bright, so the water sparkles a little.
    uint hash = iid * 2654435761u;
    float sparkle = 0.55 + float(hash >> 24) / 255.0 * 0.9;
    Glow g;
    g.uv = float2(uv.x < 0.0 ? -1.0 : 1.0, uv.y);
    g.pos = u.proj * float4(c + float3(offset, 0.0), 1.0);
    // The light is spread over the trail, so a moving drop isn't brighter for being longer,
    // only for being fast.
    g.light = colour * u.water.w * sparkle * (lit + rim) * (1.0 + 1.5 * fast + 2.0 * foam) * fade
            * (hidden ? 0.05 : 1.0) * r / (r + trail * 0.6);
    return g;
}

fragment half4 glow_fs(Glow g [[stage_in]]) {
    float r2 = dot(g.uv, g.uv);
    float soft = exp(-r2 * 4.0) * 0.6 + exp(-r2 * 25.0) * 1.5;
    return half4(half3(g.light * soft), 0.0h);
}

// A quarter of the size, for the bloom.
kernel void bloom_down(texture2d<float, access::read> src [[texture(0)]],
                       texture2d<float, access::write> dst [[texture(1)]],
                       uint2 gid [[thread_position_in_grid]]) {
    if (gid.x >= dst.get_width() || gid.y >= dst.get_height()) return;
    uint2 last = uint2(src.get_width(), src.get_height()) - 1;
    float3 sum = float3(0.0);
    for (uint i = 0; i < 4; i++)
    for (uint j = 0; j < 4; j++) {
        sum += src.read(min(gid * 4 + uint2(i, j), last)).rgb;
    }
    dst.write(float4(sum / 16.0, 1.0), gid);
}

// One direction of a Gaussian blur.
kernel void bloom_blur(texture2d<float, access::read> src [[texture(0)]],
                       texture2d<float, access::write> dst [[texture(1)]],
                       constant int2 &dir [[buffer(0)]],
                       uint2 gid [[thread_position_in_grid]]) {
    int2 size = int2(src.get_width(), src.get_height());
    if (int(gid.x) >= size.x || int(gid.y) >= size.y) return;
    float3 sum = float3(0.0);
    float total = 0.0;
    for (int i = -12; i <= 12; i++) {
        int2 q = clamp(int2(gid) + dir * i, int2(0), size - 1);
        float w = exp(-float(i * i) / 50.0);
        sum += src.read(uint2(q)).rgb * w;
        total += w;
    }
    dst.write(float4(sum / total, 1.0), gid);
}

struct Full {
    float4 pos [[position]];
    float2 uv;
};

vertex Full full_vs(uint vid [[vertex_id]]) {
    float2 p = float2(float((vid << 1) & 2), float(vid & 2));
    Full f;
    f.pos = float4(p * 2.0 - 1.0, 0.0, 1.0);
    f.uv = float2(p.x, 1.0 - p.y);
    return f;
}

// A dark backdrop, a faint violet haze of the water's colour around the planet, the planet itself
// dark with a rim of light, and over it the drops' light and its bloom.
fragment float4 composite_fs(Full f [[stage_in]],
                             texture2d<float, access::read> glow [[texture(0)]],
                             texture2d<float> bloom [[texture(1)]],
                             constant View &u [[buffer(0)]]) {
    constexpr sampler linear(filter::linear, address::clamp_to_edge);
    uint2 px = uint2(f.pos.xy);
    float2 uv = (float2(px) + 0.5) * u.screen.zw;
    float3 eye = u.eye.xyz;
    float3 dir = normalize((u.inv_view * float4(view_pos(uv, 1.0, u), 0.0)).xyz);
    float3 water = u.water.rgb;

    float3 colour = mix(float3(0.012, 0.012, 0.024), float3(0.003, 0.0035, 0.006), uv.y);
    float2 off = uv - 0.5;
    colour *= 1.0 - 0.6 * dot(off, off);

    float planet = u.drop.w;
    float b = dot(eye, dir);
    float h = b * b - (dot(eye, eye) - planet * planet);
    float miss = sqrt(max(dot(eye, eye) - b * b, 0.0)) / planet;
    colour += hue(water, -0.6) * 0.05 * exp(-max(miss - 1.0, 0.0) * 3.0);
    if (h > 0.0) {
        float3 at = eye + dir * (-b - sqrt(h));
        float3 n = at / planet;
        float rim = pow(1.0 - saturate(dot(n, -dir)), 3.0);
        float lit = saturate(dot(n, u.sun.xyz));
        colour = float3(0.002, 0.0025, 0.004) + hue(water, -0.4) * rim * 0.25 + water * lit * 0.015;
    }

    float3 light = glow.read(px).rgb + bloom.sample(linear, uv).rgb * 0.9;
    // The brightest light spills into every channel, so it turns white rather than only blue.
    light += max(max(light.r, max(light.g, light.b)) - 0.7, 0.0) * float3(0.45, 0.55, 0.6);
    colour += 1.0 - exp(-light * 1.4);
    return float4(saturate(colour), 1.0);
}
