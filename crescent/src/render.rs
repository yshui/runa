use std::{cell::RefCell, rc::Rc};

use ordered_float::NotNan;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use runa_orbiter::{
    shell::{
        buffers::{self, BufferLike as _},
        Shell,
    },
    utils::geometry::{coords, Extent, Point},
};
use runa_wayland_protocols::wayland::wl_shm::v1 as wl_shm;
use smol::channel::Receiver;
use wgpu::{include_wgsl, util::DeviceExt, WasmNotSend};
use winit::{dpi::PhysicalSize, event::Event, platform::scancode::PhysicalKeyExtScancode};

use crate::shell::DefaultShell;

#[derive(Debug, Default)]
pub struct BufferData {
    texture: RefCell<Option<wgpu::Texture>>,
}
pub type Buffer = buffers::UserBuffer<buffers::Buffer<buffers::BufferBase>, BufferData>;
pub struct Renderer {
    adapter:        wgpu::Adapter,
    device:         wgpu::Device,
    surface:        Option<wgpu::Surface<'static>>,
    queue:          wgpu::Queue,
    size:           PhysicalSize<u32>,
    pipeline:       wgpu::RenderPipeline,
    vertices:       Vec<Vertex>,
    index_buffer:   wgpu::Buffer,
    uniform_layout: wgpu::BindGroupLayout,
    texture_layout: wgpu::BindGroupLayout,
    textures:       Vec<wgpu::BindGroup>,
    uniform:        wgpu::BindGroup,
    sampler:        wgpu::Sampler,
    shell:          Rc<RefCell<DefaultShell<Buffer>>>,
    format:         wgpu::TextureFormat,
    frame_count:    usize,
}

fn shm_format_to_wgpu(format: wl_shm::enums::Format) -> wgpu::TextureFormat {
    use wl_shm::enums::Format::*;
    match format {
        Argb8888 | Xrgb8888 => wgpu::TextureFormat::Bgra8UnormSrgb,
        _ => unimplemented!("{:?}", format),
    }
}

fn get_buffer_format(buffer: &Buffer) -> wgpu::TextureFormat {
    use runa_orbiter::shell::buffers::BufferBase;
    match buffer.buffer().base() {
        BufferBase::Shm(base) => shm_format_to_wgpu(base.format()),
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 2],
    uv:       [f32; 2],
}

#[repr(C)]
#[derive(Debug, Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    dimension: [f32; 2],
}

impl Renderer {
    fn create_uniforms(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        size: PhysicalSize<u32>,
    ) -> wgpu::BindGroup {
        let uniforms = Uniforms {
            dimension: [size.width as f32, size.height as f32],
        };
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label:    None,
            contents: bytemuck::cast_slice(&[uniforms]),
            usage:    wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            entries: &[wgpu::BindGroupEntry {
                binding:  0,
                resource: uniform_buffer.as_entire_binding(),
            }],
            layout,
        })
    }

    pub async fn new<H: HasDisplayHandle + HasWindowHandle + WasmNotSend + Sync + 'static>(
        handle: H,
        size: PhysicalSize<u32>,
        shell: Rc<RefCell<DefaultShell<Buffer>>>,
    ) -> Renderer {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends:             wgpu::Backends::PRIMARY,
            dx12_shader_compiler: wgpu::Dx12Compiler::Fxc,
            flags:                wgpu::InstanceFlags::DEBUG | wgpu::InstanceFlags::VALIDATION,
            gles_minor_version:   wgpu::Gles3MinorVersion::Automatic,
        });
        tracing::debug!("instance created");
        let surface = instance.create_surface(handle).unwrap();
        tracing::debug!("surface created");
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference:       wgpu::PowerPreference::HighPerformance,
                compatible_surface:     Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .unwrap();
        tracing::debug!("adapter created");
        let format = surface.get_capabilities(&adapter).formats[0];
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    required_features: wgpu::Features::CLEAR_TEXTURE,
                    required_limits: adapter.limits(),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        tracing::debug!("device and queue created");
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label:   None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding:    0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty:         wgpu::BindingType::Buffer {
                    ty:                 wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size:   None,
                },
                count:      None,
            }],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding:    0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty:         wgpu::BindingType::Texture {
                        multisampled:   false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type:    wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count:      None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding:    1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    // This should match the filterable field of the
                    // corresponding Texture entry above.
                    ty:         wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count:      None,
                },
            ],
            label:   Some("texture_bind_group_layout"),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label:                None,
            bind_group_layouts:   &[&bind_group_layout, &texture_layout],
            push_constant_ranges: &[],
        });
        let shader = device.create_shader_module(include_wgsl!("../shaders/shader.wgsl"));
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label:         None,
            layout:        Some(&pipeline_layout),
            vertex:        wgpu::VertexState {
                module:      &shader,
                entry_point: "vs_main",
                buffers:     &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
                    step_mode:    wgpu::VertexStepMode::Vertex,
                    attributes:   &[
                        wgpu::VertexAttribute {
                            format:          wgpu::VertexFormat::Float32x2,
                            offset:          0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format:          wgpu::VertexFormat::Float32x2,
                            offset:          std::mem::size_of::<[f32; 2]>() as wgpu::BufferAddress,
                            shader_location: 1,
                        },
                    ],
                }],
            },
            fragment:      Some(wgpu::FragmentState {
                module:      &shader,
                entry_point: "fg_main",
                targets:     &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive:     wgpu::PrimitiveState {
                topology:           wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face:         wgpu::FrontFace::Ccw,
                cull_mode:          None,
                unclipped_depth:    false,
                polygon_mode:       wgpu::PolygonMode::Fill,
                conservative:       false,
            },
            depth_stencil: None,
            multiview:     None,
            multisample:   wgpu::MultisampleState::default(),
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: None,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let default_config = surface
            .get_default_config(&adapter, size.width, size.height)
            .expect("adapter doesn't support surface");
        surface.configure(&device, &wgpu::SurfaceConfiguration {
            format,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            ..default_config
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label:    Some("Index Buffer"),
            contents: bytemuck::cast_slice(&[0u16, 1, 2, 0, 2, 3]),
            usage:    wgpu::BufferUsages::INDEX,
        });
        Renderer {
            uniform: Self::create_uniforms(&device, &bind_group_layout, size),
            uniform_layout: bind_group_layout,
            texture_layout,
            textures: Vec::new(),
            pipeline,
            device,
            surface: Some(surface),
            queue,
            sampler,
            size,
            shell: shell.clone(),
            vertices: Vec::new(),
            index_buffer,
            format,
            frame_count: 0,
            adapter,
        }
    }

    fn render(&mut self, output: wgpu::SurfaceTexture) {
        use runa_orbiter::{
            shell::surface::roles::subsurface::subsurface_iter, utils::geometry::coords::Map as _,
        };
        let shell = self.shell.borrow();
        let output_scale = shell.scale_f32().map(|f| NotNan::try_from(f).unwrap());
        self.vertices.clear();
        self.textures.clear();
        for window in shell.stack() {
            tracing::trace!(?window, "rendering window");
            let window_scale = self
                .shell
                .borrow()
                .get(window.surface_state)
                .buffer_scale_f32();
            for (subsurface, offset) in subsurface_iter(window.surface_state, &*shell) {
                let relative_scale = output_scale / window_scale;
                let state = shell.get(subsurface);
                let Some(buffer) = state.buffer() else {
                    continue
                };
                let raw_dimensions = buffer.dimension();
                // Scale the buffer size to output scale
                let dimensions = raw_dimensions.map::<coords::Screen>(|dim| {
                    let dim = (dim.to() * relative_scale).floor();
                    Extent::new(dim.w.into_inner() as u32, dim.h.into_inner() as u32)
                });
                tracing::trace!(
                    ?offset,
                    ?raw_dimensions,
                    ?dimensions,
                    ?relative_scale,
                    ?window_scale,
                    "rendering subsurface {:p}",
                    buffer
                );
                // Scale the offset to output scale, and Y-flip it.
                let offset = offset.map(|o| {
                    let mut o = o.to() * output_scale;
                    o.y = -o.y;
                    let o = Point::new(o.x.into_inner() as i32, o.y.into_inner() as i32);
                    o + window.position
                });
                let mut texture = buffer.data.texture.borrow_mut();
                let texture = texture.get_or_insert_with(|| {
                    self.device.create_texture(&wgpu::TextureDescriptor {
                        label:           None,
                        dimension:       wgpu::TextureDimension::D2,
                        format:          get_buffer_format(buffer),
                        mip_level_count: 1,
                        sample_count:    1,
                        size:            wgpu::Extent3d {
                            width:                 raw_dimensions.w,
                            height:                raw_dimensions.h,
                            depth_or_array_layers: 1,
                        },
                        usage:           wgpu::TextureUsages::TEXTURE_BINDING |
                            wgpu::TextureUsages::COPY_DST,
                        view_formats:    &[],
                    })
                });
                if buffer.get_damage() {
                    // Upload the texture
                    tracing::trace!("uploading texture for buffer {}", buffer.object_id());
                    buffer.clear_damage();
                    match buffer.buffer().base() {
                        buffers::BufferBase::Shm(shm_buffer) => {
                            let pool = shm_buffer.pool();
                            let data = unsafe { pool.map() };
                            let offset = shm_buffer.offset() as usize;
                            let size = shm_buffer.stride() as usize * raw_dimensions.h as usize;
                            #[cfg(feature = "dump_texture")]
                            {
                                use std::{fs::File, io::BufWriter};
                                let dump_path = std::path::Path::new(&format!(
                                    "crescent-dump-{}-{:p}.png",
                                    self.frame_count, buffer
                                ))
                                .to_owned();
                                let ref mut file = BufWriter::new(File::create(dump_path).unwrap());
                                let mut encoder =
                                    png::Encoder::new(file, raw_dimensions.w, raw_dimensions.h);
                                encoder.set_color(png::ColorType::Rgba);
                                encoder.set_depth(png::BitDepth::Eight);
                                let mut writer = encoder.write_header().unwrap();
                                writer
                                    .write_image_data(&data[offset..offset + size])
                                    .unwrap();
                            }
                            self.queue.write_texture(
                                wgpu::ImageCopyTexture {
                                    texture,
                                    mip_level: 0,
                                    origin: wgpu::Origin3d::ZERO,
                                    aspect: wgpu::TextureAspect::All,
                                },
                                &data[offset..offset + size],
                                wgpu::ImageDataLayout {
                                    offset:         0,
                                    // TODO: reject 0 stride
                                    bytes_per_row:  Some(shm_buffer.stride() as u32),
                                    // TODO: reject 0 height
                                    rows_per_image: Some(raw_dimensions.h),
                                },
                                wgpu::Extent3d {
                                    width:                 raw_dimensions.w,
                                    height:                raw_dimensions.h,
                                    depth_or_array_layers: 1,
                                },
                            );
                        },
                    }
                    buffer.buffer().release();
                }
                let texture_bind_group =
                    self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label:   None,
                        layout:  &self.texture_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding:  0,
                                resource: wgpu::BindingResource::TextureView(
                                    &texture.create_view(&wgpu::TextureViewDescriptor::default()),
                                ),
                            },
                            wgpu::BindGroupEntry {
                                binding:  1,
                                resource: wgpu::BindingResource::Sampler(&self.sampler),
                            },
                        ],
                    });
                self.textures.push(texture_bind_group);
                self.vertices.extend_from_slice(&[
                    Vertex {
                        position: [offset.x as f32, offset.y as f32],
                        uv:       [0., 0.],
                    },
                    Vertex {
                        position: [offset.x as f32 + dimensions.w as f32, offset.y as f32],
                        uv:       [1., 0.],
                    },
                    Vertex {
                        position: [
                            offset.x as f32 + dimensions.w as f32,
                            offset.y as f32 - dimensions.h as f32,
                        ],
                        uv:       [1., 1.],
                    },
                    Vertex {
                        position: [offset.x as f32, offset.y as f32 - dimensions.h as f32],
                        uv:       [0., 1.],
                    },
                ]);
            }
        }
        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label:    Some("Vertex Buffer"),
                contents: bytemuck::cast_slice(&self.vertices),
                usage:    wgpu::BufferUsages::VERTEX,
            });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            // clear the buffer
            encoder.clear_texture(&output.texture, &Default::default());
            let view = output
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());
            for (i, texture) in self.textures.iter().enumerate() {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label:                    None,
                    color_attachments:        &[Some(wgpu::RenderPassColorAttachment {
                        view:           &view,
                        resolve_target: None,
                        ops:            wgpu::Operations {
                            load:  wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes:         None,
                    occlusion_query_set:      None,
                });
                pass.set_pipeline(&self.pipeline);
                pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                pass.set_vertex_buffer(
                    0,
                    vertex_buffer.slice((i * 4 * std::mem::size_of::<Vertex>()) as u64..),
                );
                pass.set_bind_group(0, &self.uniform, &[]);
                pass.set_bind_group(1, texture, &[]);
                pass.draw_indexed(0..6, 0, 0..1);
            }
        }
        self.queue.submit(Some(encoder.finish()));
        output.present();
        self.frame_count += 1;
    }

    pub async fn render_loop(mut self, event_rx: Receiver<Event<()>>) -> ! {
        tracing::debug!("Start render loop");
        let mut pending_size: Option<PhysicalSize<u32>> = None;
        let (remote_tx, rx) = smol::channel::bounded(1);
        let (tx, remote_rx) = smol::channel::bounded(1);
        std::thread::spawn(move || loop {
            let Ok(surface): Result<wgpu::Surface, _> = smol::block_on(remote_rx.recv()) else {
                break
            };
            let output = surface.get_current_texture().unwrap();
            tracing::trace!("Got surface texture");
            smol::block_on(remote_tx.send((surface, output))).unwrap();
        });
        tx.send(self.surface.take().unwrap()).await.unwrap();
        loop {
            use futures_util::future::FutureExt;
            futures_util::select! {
                texture_result = rx.recv().fuse() => {
                    let (surface, output) = texture_result.unwrap();
                    if let Some(new_size) = pending_size {
                        if new_size != self.size {
                            DefaultShell::update_size(&self.shell, Extent::new(new_size.width, new_size.height)).await;
                            drop(output);
                            // size changed, reconfigure surface
                            let default_config = surface.get_default_config(&self.adapter, new_size.width, new_size.height).expect("adapter doesn't support surface");
                            surface.configure(&self.device, &wgpu::SurfaceConfiguration {
                                format:       self.format,
                                present_mode: wgpu::PresentMode::Fifo,
                                alpha_mode:   wgpu::CompositeAlphaMode::Auto,
                                ..default_config
                            });
                            tx.try_send(surface).unwrap();
                            self.uniform = Self::create_uniforms(&self.device, &self.uniform_layout, new_size);
                            self.size = new_size;
                            pending_size = None;
                            continue
                        }
                    } else {
                        self.render(output);
                        // name the future before awaiting so self.shell isn't borrowed
                        // across the await
                        let fut = self.shell.borrow().notify_render();
                        fut.await;
                    }
                    tx.try_send(surface).unwrap();
                }
                event = event_rx.recv().fuse() => {
                    use winit::event::WindowEvent;
                    let event = event.unwrap();
                    match event {
                        Event::WindowEvent { event, .. } => {
                            match event {
                                WindowEvent::Resized(new_size) => {
                                    pending_size = Some(new_size);
                                }
                                WindowEvent::CursorMoved { position, .. } => {
                                    self.shell
                                        .borrow_mut()
                                        .pointer_motion(
                                            Point::new(
                                                NotNan::new(position.x as f32).unwrap(),
                                                NotNan::new(self.size.height as f32 - position.y as f32).unwrap(),
                                            )
                                        );
                                }
                                WindowEvent::MouseInput { state, button, .. } => {
                                    let pressed = matches!(state, winit::event::ElementState::Pressed);
                                    self.shell.borrow_mut().pointer_button(button, pressed);
                                }
                                WindowEvent::KeyboardInput { event, .. } => {
                                    let pressed = matches!(event.state, winit::event::ElementState::Pressed);
                                    self.shell.borrow_mut().key(event.physical_key.to_scancode().unwrap() as u8, pressed);
                                }
                                _ => {
                                    tracing::trace!("Unhandled window event: {:?}", event);
                                }
                            }
                        }
                        _ => {
                            tracing::trace!("Unhandled event: {:?}", event);
                            // todo
                        },
                    }
                }
            }
        }
    }
}
