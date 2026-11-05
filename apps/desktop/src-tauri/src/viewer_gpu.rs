//! Native swapchain presentation. No video pixels enter a webview.
use font8x8::UnicodeFonts;
use ss_video::{Frame, render};
use std::sync::Arc;
use tauri::Manager;

pub struct Gpu {
    window: Arc<tauri::Window>,
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: render::Renderer,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}
const SHADER: &str = "
@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var sample_image: sampler;
struct Vertex { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vertex(@builtin(vertex_index) id: u32) -> Vertex {
 let uv=vec2<f32>(f32((id << 1u) & 2u), f32(id & 2u));
 var out: Vertex; out.position=vec4<f32>(uv.x*2.0-1.0,1.0-uv.y*2.0,0.0,1.0); out.uv=uv; return out;
}
@fragment fn fragment(v: Vertex) -> @location(0) vec4<f32> { return textureSample(image,sample_image,v.uv); }
";
impl Gpu {
    pub async fn new(
        window: Arc<tauri::Window>,
        instance: wgpu::Instance,
        surface: wgpu::Surface<'static>,
    ) -> Result<Self, String> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .map_err(|e| e.to_string())?;
        let size = window.inner_size().map_err(|e| e.to_string())?;
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or("No surface configuration")?;
        if let Some(format) = surface
            .get_capabilities(&adapter)
            .formats
            .into_iter()
            .find(|f| !f.is_srgb())
        {
            config.format = format;
        }
        config.present_mode = wgpu::PresentMode::AutoNoVsync;
        config.desired_maximum_frame_latency = 1;
        surface.configure(&device, &config);
        let renderer = render::Renderer::new(&device, &queue, render::Config::new())
            .map_err(|e| e.to_string())?;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Ok(Self {
            window,
            instance,
            surface,
            device,
            queue,
            config,
            renderer,
            pipeline,
            layout,
            sampler,
        })
    }
    fn bind(&self, texture: &wgpu::Texture) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &texture.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }
    fn text(&self, text: &str) -> wgpu::Texture {
        let text: String = text.chars().take(110).collect();
        let width = (text.len() as u32 * 8).max(8);
        let height = 12;
        let mut pixels = vec![0_u8; (width * height * 4) as usize];
        for p in pixels.as_chunks_mut::<4>().0.iter_mut() {
            p.copy_from_slice(&[15, 20, 25, 240]);
        }
        for (i, c) in text.chars().enumerate() {
            if let Some(glyph) = font8x8::BASIC_FONTS.get(c) {
                for (y, row) in glyph.into_iter().enumerate() {
                    for x in 0..8 {
                        if row & (1 << x) != 0 {
                            let j = ((y + 2) * width as usize + i * 8 + x) * 4;
                            pixels[j..j + 4].copy_from_slice(&[230, 240, 250, 255]);
                        }
                    }
                }
            }
        }
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            texture.as_image_copy(),
            &pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            size,
        );
        texture
    }
    pub fn draw(&mut self, frame: Option<&Frame>, text: Option<&str>) -> Result<(), String> {
        let size = self.window.inner_size().map_err(|e| e.to_string())?;
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        if (size.width, size.height) != (self.config.width, self.config.height) {
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&self.device, &self.config);
        }
        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Lost => {
                let instance = self.instance.clone();
                let window = self.window.clone();
                let (send, receive) = std::sync::mpsc::sync_channel(1);
                self.window
                    .app_handle()
                    .run_on_main_thread(move || {
                        let _ =
                            send.send(instance.create_surface(window).map_err(|e| e.to_string()));
                    })
                    .map_err(|e| e.to_string())?;
                self.surface = receive
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .map_err(|e| e.to_string())??;
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            _ => return Err("GPU surface validation failed".into()),
        };
        let video = frame
            .map(|f| self.renderer.render(f))
            .transpose()
            .map_err(|e| e.to_string())?;
        let video_bind = video.as_ref().map(|t| self.bind(t));
        let overlay = text.map(|t| self.text(t));
        let text_bind = overlay.as_ref().map(|t| self.bind(t));
        let view = output.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            if let (Some(f), Some(bind)) = (frame, video_bind.as_ref()) {
                let (x, y, w, h) =
                    aspect_fit(f.size().width, f.size().height, size.width, size.height);
                pass.set_viewport(x, y, w, h, 0.0, 1.0);
                pass.set_bind_group(0, bind, &[]);
                pass.draw(0..3, 0..1);
            }
            if let (Some(texture), Some(bind)) = (overlay.as_ref(), text_bind.as_ref()) {
                let w = (texture.width() as f32 * 1.5).min(size.width as f32);
                pass.set_viewport(0.0, 0.0, w, 18.0_f32.min(size.height as f32), 0.0, 1.0);
                pass.set_bind_group(0, bind, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(output);
        Ok(())
    }
}
pub fn aspect_fit(vw: u32, vh: u32, ww: u32, wh: u32) -> (f32, f32, f32, f32) {
    let scale = (ww as f32 / vw.max(1) as f32).min(wh as f32 / vh.max(1) as f32);
    let (w, h) = (vw as f32 * scale, vh as f32 * scale);
    ((ww as f32 - w) / 2.0, (wh as f32 - h) / 2.0, w, h)
}
#[cfg(test)]
mod tests {
    #[test]
    fn fit_keeps_portrait_and_landscape_centered() {
        assert_eq!(
            super::aspect_fit(1920, 1080, 800, 600),
            (0.0, 75.0, 800.0, 450.0)
        );
        assert_eq!(
            super::aspect_fit(100, 200, 800, 600),
            (250.0, 0.0, 300.0, 600.0)
        );
    }
}
