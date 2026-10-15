//! Saves and restores the GL state mpv's renderer touches, so Slint's
//! femtovg renderer finds the context exactly as it left it.

use glow::HasContext;

pub struct GlState {
    program: i32,
    vao: i32,
    array_buffer: i32,
    active_texture: i32,
    texture_2d: i32,
    framebuffer: i32,
    viewport: [i32; 4],
    scissor_box: [i32; 4],
    blend: bool,
    scissor: bool,
    blend_func: [i32; 4],
}

fn nz(v: i32) -> Option<std::num::NonZeroU32> {
    std::num::NonZeroU32::new(v as u32)
}

impl GlState {
    /// # Safety
    /// The GL context behind `gl` must be current.
    pub unsafe fn save(gl: &glow::Context) -> Self {
        unsafe {
            let mut viewport = [0; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            let mut scissor_box = [0; 4];
            gl.get_parameter_i32_slice(glow::SCISSOR_BOX, &mut scissor_box);
            Self {
                program: gl.get_parameter_i32(glow::CURRENT_PROGRAM),
                vao: gl.get_parameter_i32(glow::VERTEX_ARRAY_BINDING),
                array_buffer: gl.get_parameter_i32(glow::ARRAY_BUFFER_BINDING),
                active_texture: gl.get_parameter_i32(glow::ACTIVE_TEXTURE),
                texture_2d: gl.get_parameter_i32(glow::TEXTURE_BINDING_2D),
                framebuffer: gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING),
                viewport,
                scissor_box,
                blend: gl.is_enabled(glow::BLEND),
                scissor: gl.is_enabled(glow::SCISSOR_TEST),
                blend_func: [
                    gl.get_parameter_i32(glow::BLEND_SRC_RGB),
                    gl.get_parameter_i32(glow::BLEND_DST_RGB),
                    gl.get_parameter_i32(glow::BLEND_SRC_ALPHA),
                    gl.get_parameter_i32(glow::BLEND_DST_ALPHA),
                ],
            }
        }
    }

    /// # Safety
    /// Same context as `save`, still current.
    pub unsafe fn restore(&self, gl: &glow::Context) {
        unsafe {
            gl.use_program(nz(self.program).map(glow::NativeProgram));
            gl.bind_vertex_array(nz(self.vao).map(glow::NativeVertexArray));
            gl.bind_buffer(glow::ARRAY_BUFFER, nz(self.array_buffer).map(glow::NativeBuffer));
            gl.active_texture(self.active_texture as u32);
            gl.bind_texture(glow::TEXTURE_2D, nz(self.texture_2d).map(glow::NativeTexture));
            gl.bind_framebuffer(glow::FRAMEBUFFER, nz(self.framebuffer).map(glow::NativeFramebuffer));
            let [x, y, w, h] = self.viewport;
            gl.viewport(x, y, w, h);
            let [x, y, w, h] = self.scissor_box;
            gl.scissor(x, y, w, h);
            let set = |cap, on: bool| if on { gl.enable(cap) } else { gl.disable(cap) };
            set(glow::BLEND, self.blend);
            set(glow::SCISSOR_TEST, self.scissor);
            let [a, b, c, d] = self.blend_func.map(|v| v as u32);
            gl.blend_func_separate(a, b, c, d);
        }
    }
}
