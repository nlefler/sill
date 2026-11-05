// Video: NV12 (R8 luma + RG8 chroma) to RGB. Overlay: a straight-alpha RGBA quad (cursor, HUD).

struct Quad {
    // NDC rectangle: x0, y0 (bottom), x1, y1 (top).
    rect: vec4<f32>,
    // Video: kr, kb, full range (1) or limited (0), unused.
    params: vec4<f32>,
    // Video: the part of the texture the picture covers (x, y); an imported dmabuf is the aligned surface.
    crop: vec4<f32>,
}

@group(0) @binding(0) var<uniform> q: Quad;
@group(0) @binding(1) var tex0: texture_2d<f32>;
@group(0) @binding(2) var tex1: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VOut {
    let c = vec2<f32>(f32(i & 1u), f32(i >> 1u));
    var o: VOut;
    o.pos = vec4<f32>(mix(q.rect.x, q.rect.z, c.x), mix(q.rect.w, q.rect.y, c.y), 0.0, 1.0);
    o.uv = c;
    return o;
}

@fragment
fn fs_video(i: VOut) -> @location(0) vec4<f32> {
    let dim = vec2<f32>(textureDimensions(tex0));
    let uv = clamp(i.uv * q.crop.xy, vec2<f32>(0.5) / dim, q.crop.xy - vec2<f32>(0.5) / dim);
    let y = textureSample(tex0, samp, uv).r * 255.0;
    let c = textureSample(tex1, samp, uv).rg * 255.0;
    var yy: f32;
    var cb: f32;
    var cr: f32;
    if (q.params.z < 0.5) {
        yy = (y - 16.0) / 219.0;
        cb = (c.x - 128.0) / 224.0;
        cr = (c.y - 128.0) / 224.0;
    } else {
        yy = y / 255.0;
        cb = (c.x - 128.0) / 255.0;
        cr = (c.y - 128.0) / 255.0;
    }
    let kr = q.params.x;
    let kb = q.params.y;
    let kg = 1.0 - kr - kb;
    let r = yy + 2.0 * (1.0 - kr) * cr;
    let b = yy + 2.0 * (1.0 - kb) * cb;
    let g = (yy - kr * r - kb * b) / kg;
    return vec4<f32>(clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}

@fragment
fn fs_overlay(i: VOut) -> @location(0) vec4<f32> {
    return textureSample(tex0, samp, i.uv);
}
