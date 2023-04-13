use anyhow::Result;
use bevy_asset::{AssetLoader, LoadContext, LoadedAsset};
use bevy_render::{
    mesh::{Indices, Mesh},
    render_resource::PrimitiveTopology,
};
use bevy_utils::BoxedFuture;
use thiserror::Error;
#[cfg(feature = "scene")]
use {
    bevy_asset::AssetPath,
    bevy_ecs::world::{FromWorld, World},
    bevy_hierarchy::BuildWorldChildren,
    bevy_pbr::{PbrBundle, StandardMaterial},
    bevy_render::{
        prelude::{Color, SpatialBundle},
        renderer::RenderDevice,
        texture::{CompressedImageFormats, Image, ImageType},
    },
    bevy_scene::Scene,
};

#[cfg(not(feature = "scene"))]
#[derive(Default)]
pub struct ObjLoader;

#[cfg(feature = "scene")]
pub struct ObjLoader {
    supported_compressed_formats: CompressedImageFormats,
}

impl AssetLoader for ObjLoader {
    fn load<'a>(
        &'a self,
        bytes: &'a [u8],
        load_context: &'a mut bevy_asset::LoadContext,
    ) -> BoxedFuture<'a, Result<(), anyhow::Error>> {
        Box::pin(async move {
            Ok(load_obj(
                bytes,
                load_context,
                #[cfg(feature = "scene")]
                self.supported_compressed_formats,
            )
            .await?)
        })
    }

    fn extensions(&self) -> &[&str] {
        static EXTENSIONS: &[&str] = &["obj"];
        EXTENSIONS
    }
}

#[cfg(feature = "scene")]
impl FromWorld for ObjLoader {
    fn from_world(world: &mut World) -> Self {
        let supported_compressed_formats = match world.get_resource::<RenderDevice>() {
            Some(render_device) => CompressedImageFormats::from_features(render_device.features()),

            None => CompressedImageFormats::all(),
        };
        Self {
            supported_compressed_formats,
        }
    }
}

#[derive(Error, Debug)]
pub enum ObjError {
    #[error("Invalid OBJ file: {0}")]
    TobjError(#[from] tobj::LoadError),
}

async fn load_obj<'a, 'b>(
    bytes: &'a [u8],
    load_context: &'a mut LoadContext<'b>,
    #[cfg(feature = "scene")] supported_compressed_formats: CompressedImageFormats,
) -> Result<(), ObjError> {
    #[cfg(not(feature = "scene"))]
    {
        let mesh = load_obj_from_bytes(bytes)?;
        load_context.set_default_asset(LoadedAsset::new(mesh));
    }
    #[cfg(feature = "scene")]
    {
        let scene = load_obj_from_bytes(bytes, load_context, supported_compressed_formats).await?;
        load_context.set_default_asset(LoadedAsset::new(scene));
    }
    Ok(())
}

#[cfg(feature = "scene")]
async fn load_obj_from_bytes<'a, 'b>(
    mut bytes: &'a [u8],
    load_context: &'a mut LoadContext<'b>,
    supported_compressed_formats: CompressedImageFormats,
) -> Result<Scene, ObjError> {
    println!("Loading scene");
    let options = tobj::GPU_LOAD_OPTIONS;
    let asset_io = &load_context.asset_io();
    let obj = tobj::load_obj_buf_async(&mut bytes, &options, |p| async move {
        // TODO(luca) error handling here
        let bytes = asset_io.load_path(std::path::Path::new(&p)).await.unwrap();
        tobj::load_mtl_buf(&mut bytes.as_slice())
    })
    .await?;
    let models = obj.0;
    // TODO(luca) should we just populate standard materials here instead?
    let materials = obj.1?;
    let mut world = World::default();
    let world_id = world.spawn(SpatialBundle::VISIBLE_IDENTITY).id();
    for mat in &materials {
        println!("Found material");
        let mut material = StandardMaterial {
            base_color: Color::rgb(mat.diffuse[0], mat.diffuse[1], mat.diffuse[2]),
            ..Default::default()
        };
        if !mat.diffuse_texture.is_empty() {
            println!("Found texture");
            // Load image
            let path = std::path::Path::new(&mat.diffuse_texture);
            let filename = path.file_name().unwrap().to_str().unwrap();
            // TODO(luca) error handling in load_path

            let bytes = load_context.asset_io().load_path(&path).await.unwrap();
            let extension = ImageType::Extension(path.extension().unwrap().to_str().unwrap());
            // TODO(luca) confirm value of is_srgb
            let is_srgb = false;
            let img = Image::from_buffer(&bytes, extension, supported_compressed_formats, is_srgb)
                .unwrap();
            let texture_asset_path = AssetPath::new_ref(load_context.path(), Some(filename));
            material.base_color_texture = Some(load_context.get_handle(texture_asset_path));
            load_context.set_labeled_asset(filename, LoadedAsset::new(img));
            load_context.set_labeled_asset(&mat.name, LoadedAsset::new(material));
        }
    }
    for model in models {
        let vertex_position: Vec<[f32; 3]> = model
            .mesh
            .positions
            .chunks_exact(3)
            .map(|v| [v[0], v[1], v[2]])
            .collect();
        let vertex_normal: Vec<[f32; 3]> = model
            .mesh
            .normals
            .chunks_exact(3)
            .map(|n| [n[0], n[1], n[2]])
            .collect();
        let vertex_texture: Vec<[f32; 2]> = model
            .mesh
            .texcoords
            .chunks_exact(2)
            .map(|t| [t[0], 1.0 - t[1]])
            .collect();

        let mut mesh = Mesh::new(PrimitiveTopology::TriangleList);

        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vertex_position);
        if !vertex_texture.is_empty() {
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vertex_texture);
        }

        if !vertex_normal.is_empty() {
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vertex_normal);
        } else {
            mesh.compute_flat_normals();
        }

        mesh.set_indices(Some(Indices::U32(model.mesh.indices)));
        load_context.set_labeled_asset(&model.name, LoadedAsset::new(mesh));
        // Now create the material
        let mesh_asset_path = AssetPath::new_ref(load_context.path(), Some(&model.name));
        let pbr_id = if let Some(mat_name) = model
            .mesh
            .material_id
            .and_then(|id| materials.get(id))
            .map(|mat| mat.name.clone())
        {
            let material_asset_path = AssetPath::new_ref(load_context.path(), Some(&mat_name));
            println!("Fetching material");
            world
                .spawn(PbrBundle {
                    mesh: load_context.get_handle(mesh_asset_path),
                    material: load_context.get_handle(material_asset_path),
                    ..Default::default()
                })
                .id()
        } else {
            world
                .spawn(PbrBundle {
                    mesh: load_context.get_handle(mesh_asset_path),
                    ..Default::default()
                })
                .id()
        };
        world.entity_mut(world_id).push_children(&[pbr_id]);
    }

    println!("Finished loading scene");
    dbg!(&world);
    Ok(Scene::new(world))
}

#[cfg(not(feature = "scene"))]
pub fn load_obj_from_bytes(mut bytes: &[u8]) -> Result<Mesh, ObjError> {
    let options = tobj::GPU_LOAD_OPTIONS;
    let obj = tobj::load_obj_buf(&mut bytes, &options, |_m| {
        Err(tobj::LoadError::OpenFileFailed)
    })?;
    let mut indices = Vec::new();
    let mut vertex_position = Vec::new();
    let mut vertex_normal = Vec::new();
    let mut vertex_texture = Vec::new();
    for model in obj.0 {
        indices.reserve(model.mesh.indices.len());
        vertex_position.reserve(model.mesh.positions.len() / 3);
        vertex_normal.reserve(model.mesh.normals.len() / 3);
        vertex_texture.reserve(model.mesh.texcoords.len() / 2);
        vertex_position.extend(
            model
                .mesh
                .positions
                .chunks_exact(3)
                .map(|v| [v[0], v[1], v[2]]),
        );
        vertex_normal.extend(
            model
                .mesh
                .normals
                .chunks_exact(3)
                .map(|n| [n[0], n[1], n[2]]),
        );
        vertex_texture.extend(
            model
                .mesh
                .texcoords
                .chunks_exact(2)
                .map(|t| [t[0], 1.0 - t[1]]),
        );
        indices.extend(model.mesh.indices);
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList);

    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vertex_position);
    if !vertex_texture.is_empty() {
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vertex_texture);
    }

    if !vertex_normal.is_empty() {
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vertex_normal);
    } else {
        mesh.compute_flat_normals();
    }

    mesh.set_indices(Some(Indices::U32(indices)));

    Ok(mesh)
}
