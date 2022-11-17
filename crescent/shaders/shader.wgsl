@group(1) @binding(0) var texture: texture_2d<f32>;
@group(1) @binding(1) var s: sampler;
@group(0) @binding(0) var<uniform> screen_size: vec2<f32>;

struct VertexOutput {
	@builtin(position) position: vec4<f32>,
	@location(0) uv: vec2<f32>,
}

struct VertexInput {
	@location(0) position: vec2<f32>,
	@location(1) uv: vec2<f32>,
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.position = vec4<f32>(input.position / screen_size * 2.0 - 1.0, 0.0, 1.0);
    output.uv = input.uv;
    return output;
}

@fragment
fn fg_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(texture, s, input.uv);
    //return vec4<f32>(1., 1., 1., 1.);
}
