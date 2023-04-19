use anyhow::Result;
use bevy_asset::{Handle, LoadContext, LoadedAsset};
use bevy_ecs::world::{FromWorld, World};
use bevy_hierarchy::BuildWorldChildren;
use bevy_pbr::{PbrBundle, StandardMaterial};
use bevy_render::{
    mesh::{Indices, Mesh},
    prelude::{Color, SpatialBundle},
    render_resource::PrimitiveTopology,
    renderer::RenderDevice,
    texture::{CompressedImageFormats, Image, ImageType},
};
use bevy_scene::Scene;
use std::path::PathBuf;
use thiserror::Error;

fn material_label(idx: usize) -> String {
    "Material".to_owned() + &idx.to_string()
}

fn mesh_label(idx: usize) -> String {
    "Mesh".to_owned() + &idx.to_string()
}

impl FromWorld for super::ObjLoader {
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
    #[error("Invalid image file for texture: {0}")]
    InvalidImageFile(PathBuf),
    #[error("Asset reading failed: {0}")]
    AssetIOError(#[from] bevy_asset::AssetIoError),
    #[error("Texture conversion failed: {0}")]
    TextureError(#[from] bevy_render::texture::TextureError),
}

pub(super) async fn load_obj<'a, 'b>(
    bytes: &'a [u8],
    load_context: &'a mut LoadContext<'b>,
    supported_compressed_formats: CompressedImageFormats,
) -> Result<(), ObjError> {
    let obj = load_obj_scene(bytes, load_context, supported_compressed_formats).await?;
    load_context.set_default_asset(LoadedAsset::new(obj));
    Ok(())
}

async fn load_texture_image<'a, 'b>(
    image_path: &'a str,
    load_context: &'a mut LoadContext<'b>,
    supported_compressed_formats: CompressedImageFormats,
) -> Result<Image, ObjError> {
    let mut path = load_context.path().to_owned();
    path.set_file_name(image_path);
    let extension = ImageType::Extension(
        path.extension()
            .and_then(|e| e.to_str())
            .ok_or(ObjError::InvalidImageFile(path.to_path_buf()))?,
    );
    let bytes = load_context.asset_io().load_path(&path).await?;
    // TODO(luca) confirm value of is_srgb
    let is_srgb = true;
    Ok(Image::from_buffer(
        &bytes,
        extension,
        supported_compressed_formats,
        is_srgb,
    )?)
}

async fn load_obj_data<'a, 'b>(
    mut bytes: &'a [u8],
    load_context: &'a mut LoadContext<'b>,
) -> tobj::LoadResult {
    let options = tobj::GPU_LOAD_OPTIONS;
    let asset_io = &load_context.asset_io();
    let ctx_path = load_context.path();
    tobj::load_obj_buf_async(&mut bytes, &options, |p| async move {
        let mut asset_path = ctx_path.to_owned();
        asset_path.set_file_name(p);
        asset_io
            .load_path(&asset_path)
            .await
            .map_or(Err(tobj::LoadError::OpenFileFailed), |bytes| {
                tobj::load_mtl_buf(&mut bytes.as_slice())
            })
    })
    .await
}

async fn load_mat_texture<'a, 'b>(
    texture: &String,
    load_context: &'a mut LoadContext<'b>,
    supported_compressed_formats: CompressedImageFormats,
) -> Result<Option<Handle<Image>>, ObjError> {
    if !texture.is_empty() {
        let handle = if load_context.has_labeled_asset(texture) {
            load_context.get_handle(texture)
        } else {
            let img =
                load_texture_image(texture, load_context, supported_compressed_formats).await?;
            load_context.set_labeled_asset(texture, LoadedAsset::new(img))
        };
        Ok(Some(handle))
    } else {
        Ok(None)
    }
}

async fn load_obj_scene<'a, 'b>(
    bytes: &'a [u8],
    load_context: &'a mut LoadContext<'b>,
    supported_compressed_formats: CompressedImageFormats,
) -> Result<Scene, ObjError> {
    let (models, materials) = load_obj_data(bytes, load_context).await?;
    let materials = materials?;

    let mut mat_handles = Vec::with_capacity(materials.len());
    for (mat_idx, mat) in materials.into_iter().enumerate() {
        // TODO(luca) check other material properties
        let material = StandardMaterial {
            base_color: Color::rgb(mat.diffuse[0], mat.diffuse[1], mat.diffuse[2]),
            base_color_texture: load_mat_texture(
                &mat.diffuse_texture,
                load_context,
                supported_compressed_formats,
            )
            .await?,
            normal_map_texture: load_mat_texture(
                &mat.normal_texture,
                load_context,
                supported_compressed_formats,
            )
            .await?,
            ..Default::default()
        };
        mat_handles.push(load_context.set_labeled_asset(&material_label(mat_idx), LoadedAsset::new(material)));
    }

    let mut world = World::default();
    let world_id = world.spawn(SpatialBundle::INHERITED_IDENTITY).id();
    for (model_idx, model) in models.into_iter().enumerate() {
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
        mesh.set_indices(Some(Indices::U32(model.mesh.indices)));

        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vertex_position);
        if !vertex_texture.is_empty() {
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vertex_texture);
        }

        if !vertex_normal.is_empty() {
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vertex_normal);
        } else {
            mesh.duplicate_vertices();
            mesh.compute_flat_normals();
        }

        let mesh_handle =
            load_context.set_labeled_asset(&mesh_label(model_idx), LoadedAsset::new(mesh));

        // Now assign the material
        let pbr_id = if let Some(mat_id) = model.mesh.material_id {
            world
                .spawn(PbrBundle {
                    mesh: mesh_handle,
                    material: mat_handles[mat_id].clone(),
                    ..Default::default()
                })
                .id()
        } else {
            world
                .spawn(PbrBundle {
                    mesh: mesh_handle,
                    ..Default::default()
                })
                .id()
        };
        world.entity_mut(world_id).push_children(&[pbr_id]);
    }

    Ok(Scene::new(world))
}
