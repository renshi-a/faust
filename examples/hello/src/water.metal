// The water's shaders, compiled by Metal when the app starts (no Xcode needed).
//
// Simulation: the material point method (MLS-MPM) on a 64³ grid. Each step, the drops hand
// their mass and momentum to the grid (p2g_*), the grid moves them on (grid_update), and the
// drops take their new velocity and its gradient back (g2p).
//
// Rendering, in screen space: the drops are drawn as little spheres into a depth texture and a
// thickness texture, the depth is smoothed into one surface (smooth), and the surface is lit
// with reflection, refraction and the colour water takes on with depth (composite).

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
    float4 centre;  // xyz: the ball's centre; w: the pull toward it
    float4 impulse; // xyz: where a ripple starts; w: how hard
    float4 fluid;   // x: time step, y: rest density, z: stiffness, w: viscosity
    float4 misc;    // x: fixed-point scale, y: outward burst, z: ripple size
    float4 swirl;   // xyz: the axis the water turns about, w: how fast (radians per time)
    float4 tide;    // xyz: the axis the water is drawn out along, w: how hard
    uint4 size;     // x: drops, y: grid side
};

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
    float pressure = clamp(s.fluid.z * (pow(density / s.fluid.y, 4.0) - 1.0), -0.5, 200.0);
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

// Velocities from momentum, the pull toward the centre, ripples, and the walls of the grid.
kernel void grid_update(device Cell *cells [[buffer(0)]],
                        device float4 *velocity [[buffer(1)]],
                        constant Sim &s [[buffer(2)]],
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
    // Like a small planet: the pull grows from nothing at the centre.
    v -= outward * s.centre.w * min(r / 6.0, 1.0) * dt;
    if (s.impulse.w > 0.0) {
        float3 d = pos - s.impulse.xyz;
        float size = s.misc.z;
        v += outward * s.impulse.w * exp(-dot(d, d) / (size * size)) * dt;
    }
    v += outward * s.misc.y * min(r / 6.0, 1.0) * dt;
    // A tide, as from a moon circling the ball: it draws the water out along one axis and
    // squeezes it across, and as the axis turns the ball keeps changing shape where it is.
    v += s.tide.w * (3.0 * dot(out, s.tide.xyz) * s.tide.xyz - out) * dt;
    // A slow turn about an axis that wanders, so the water never quite settles: the water is
    // eased toward turning at that rate, which also keeps it from speeding up without end.
    v += (cross(s.swirl.xyz, out) * s.swirl.w - v) * 0.02 * dt;
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
    q.x = clamp(q.x + v * s.fluid.x, float3(2.0), float3(float(side) - 3.0));
    p[id] = q;
}

// ---------------------------------------------------------------------------------------------
// Rendering

struct View {
    float4x4 view;
    float4x4 proj;
    float4x4 inv_view;
    float4x4 inv_proj;
    float4 screen; // width, height, 1 / width, 1 / height (pixels)
    float4 drop;   // x: drawn radius, y: grid-to-world scale, z: grid centre, w: smoothing radius
    float4 sun;    // xyz: toward the sun
    float4 eye;    // xyz: the camera
    float4 water;  // rgb: the water's colour, w: thickness scale
};

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

// The front of each drop's sphere: its distance from the camera, and the depth to test.
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

// How much water each drop puts in front of the pixel; the passes add up.
fragment float thickness_fs(Sprite s [[stage_in]], constant View &u [[buffer(1)]]) {
    float r2 = dot(s.uv, s.uv);
    if (r2 > 1.0) discard_fragment();
    return 2.0 * sqrt(1.0 - r2) * u.drop.x * u.water.w;
}

// One direction of a bilateral blur: neighbours count less the further they are in depth, so
// the drops melt into a surface but the edge of the ball stays sharp.
kernel void smooth(texture2d<float, access::read> src [[texture(0)]],
                   texture2d<float, access::write> dst [[texture(1)]],
                   constant View &u [[buffer(0)]],
                   constant int2 &dir [[buffer(1)]],
                   uint2 gid [[thread_position_in_grid]]) {
    int2 size = int2(src.get_width(), src.get_height());
    if (int(gid.x) >= size.x || int(gid.y) >= size.y) return;
    float d = src.read(gid).r;
    if (d <= 0.0) {
        dst.write(float4(0.0), gid);
        return;
    }
    float pixels = u.drop.w * u.proj[1][1] * 0.5 * u.screen.y / d;
    int r = clamp(int(pixels), 1, 20);
    float sigma = max(pixels * 0.5, 0.5);
    float range = u.drop.x * 1.5;
    float sum = 0.0;
    float total = 0.0;
    for (int i = -r; i <= r; i++) {
        int2 q = int2(gid) + dir * i;
        if (q.x < 0 || q.y < 0 || q.x >= size.x || q.y >= size.y) continue;
        float s = src.read(uint2(q)).r;
        if (s <= 0.0) continue;
        float w = exp(-float(i * i) / (2.0 * sigma * sigma)) * exp(-(s - d) * (s - d) / (2.0 * range * range));
        sum += s * w;
        total += w;
    }
    dst.write(float4(sum / max(total, 1e-6)), gid);
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

// What the water reflects and refracts: a white photo studio, a grey floor far below, a large
// softbox toward the sun and a strip light opposite it. (What the camera sees around the ball is
// plain white.)
static float3 studio(float3 d, constant View &u) {
    float3 c = d.y >= 0.0 ? mix(float3(0.80), float3(1.0), smoothstep(0.0, 0.8, d.y))
                          : mix(float3(0.75), float3(0.38), smoothstep(0.0, -0.3, d.y));
    float3 key = u.sun.xyz;
    float k = dot(d, key);
    c += float3(1.0, 0.98, 0.95) * (smoothstep(0.93, 0.97, k) * 2.5 + pow(max(k, 0.0), 6.0) * 0.25);
    // The strip: a narrow band, upright, on the other side.
    float3 strip = normalize(float3(-key.x, 0.2, -key.z));
    float side = dot(d, strip);
    float across = abs(dot(d, normalize(cross(strip, float3(0.0, 1.0, 0.0)))));
    c += float3(1.6) * smoothstep(0.75, 0.85, side) * smoothstep(0.12, 0.06, across);
    return c;
}

static float3 view_pos(float2 uv, float d, constant View &u) {
    float2 ndc = float2(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    float4 r = u.inv_proj * float4(ndc, 1.0, 1.0);
    float3 ray = r.xyz / r.w;
    return ray * (d / -ray.z);
}

fragment float4 composite_fs(Full f [[stage_in]],
                             texture2d<float, access::read> depth [[texture(0)]],
                             texture2d<float, access::read> thick [[texture(1)]],
                             constant View &u [[buffer(0)]]) {
    uint2 px = uint2(f.pos.xy);
    float2 uv = (float2(px) + 0.5) * u.screen.zw;
    float3 eye = u.eye.xyz;
    float3 ray = view_pos(uv, 1.0, u);
    float3 dir = normalize((u.inv_view * float4(ray, 0.0)).xyz);
    float d = depth.read(px).r;
    if (d <= 0.0) return float4(1.0);

    // The surface's normal, from the neighbour on whichever side is closer in depth.
    int2 size = int2(depth.get_width(), depth.get_height());
    float3 p = view_pos(uv, d, u);
    float3 ddx = float3(0.0);
    float3 ddy = float3(0.0);
    float best_x = 1e9;
    float best_y = 1e9;
    for (int s = -1; s <= 1; s += 2) {
        int2 qx = clamp(int2(px) + int2(s, 0), int2(0), size - 1);
        int2 qy = clamp(int2(px) + int2(0, s), int2(0), size - 1);
        float dx = depth.read(uint2(qx)).r;
        float dy = depth.read(uint2(qy)).r;
        if (dx > 0.0 && abs(dx - d) < best_x) {
            best_x = abs(dx - d);
            ddx = (view_pos((float2(qx) + 0.5) * u.screen.zw, dx, u) - p) * float(s);
        }
        if (dy > 0.0 && abs(dy - d) < best_y) {
            best_y = abs(dy - d);
            ddy = (view_pos((float2(qy) + 0.5) * u.screen.zw, dy, u) - p) * float(s);
        }
    }
    float3 n = cross(ddx, ddy);
    n = length(n) > 1e-12 ? normalize(n) : float3(0.0, 0.0, 1.0);
    if (n.z < 0.0) n = -n;
    float3 N = normalize((u.inv_view * float4(n, 0.0)).xyz);
    float3 P = (u.inv_view * float4(p, 1.0)).xyz;
    float3 V = normalize(eye - P);

    float t = thick.read(px).r;
    float3 absorb = exp(-t * (1.0 - u.water.rgb) * 4.0);
    float3 refracted = refract(-V, N, 1.0 / 1.33);
    // Bent less than a real ball bends it, so the white behind shows through the middle.
    float3 behind = studio(normalize(mix(dir, refracted, 0.35)), u);
    // Light scattered inside: deep water glows faintly in its own colour.
    float3 body = behind * absorb + u.water.rgb * u.water.rgb * 0.1 * (1.0 - absorb);
    float3 reflected = studio(reflect(-V, N), u);
    float fresnel = 0.02 + 0.98 * pow(1.0 - saturate(dot(N, V)), 5.0);
    float3 h = normalize(V + u.sun.xyz);
    float glint = pow(saturate(dot(N, h)), 900.0) * 18.0 + pow(saturate(dot(N, h)), 60.0) * 0.15;
    float3 colour = mix(body, reflected, fresnel) + float3(1.0, 0.95, 0.88) * glint;
    // Where there is only a film of water, the edge fades into the white around it.
    float edge = smoothstep(0.004, 0.04, t);
    return float4(mix(float3(1.0), saturate(colour), edge), 1.0);
}
