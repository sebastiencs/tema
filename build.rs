use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    let shader_dir = Path::new("src/render/vulkan/shaders");
    println!("cargo:rerun-if-changed=src/render/vulkan/shaders");
    println!("cargo:rerun-if-changed=src/render/shaders_common/color.glsl");
    println!("cargo:rerun-if-changed=src/shaders/texture_clip.frag");
    println!("cargo:rerun-if-changed=src/render/shaders_common/rounded_rect.glsl");
    println!("cargo:rerun-if-changed=src/shaders/border.frag");
    println!("cargo:rerun-if-changed=src/shaders/shadow.frag");
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let shaders = shader_sources(shader_dir);
    let generated =
        if shaders.is_empty() { String::new() } else { compile_shaders(&shaders, &out_dir) };
    fs::write(out_dir.join("vulkan_shaders.rs"), generated).expect("write vulkan shader module");
}

fn shader_sources(dir: &Path) -> Vec<(PathBuf, shaderc_dyn::ShaderKind)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut shaders = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let kind = match path.extension().and_then(|extension| extension.to_str()) {
            Some("vert") => shaderc_dyn::ShaderKind::Vertex,
            Some("frag") => shaderc_dyn::ShaderKind::Fragment,
            Some("comp") => shaderc_dyn::ShaderKind::Compute,
            _ => continue,
        };
        shaders.push((path, kind));
    }
    shaders.sort_by(|left, right| left.0.cmp(&right.0));
    shaders
}

fn compile_shaders(shaders: &[(PathBuf, shaderc_dyn::ShaderKind)], out_dir: &Path) -> String {
    // shaderc-dyn opens libshaderc_shared.so. The runtime package on this
    // system ships the versioned soname and leaves the unversioned name to
    // the development package, so point at the library that is installed.
    if env::var_os("SHADERC_LIB_PATH").is_none() {
        let candidates = [
            "/lib64/libshaderc_shared.so",
            "/usr/lib64/libshaderc_shared.so",
            "/lib/libshaderc_shared.so",
            "/lib64/libshaderc_shared.so.1",
            "/usr/lib64/libshaderc_shared.so.1",
            "/lib/libshaderc_shared.so.1",
        ];
        if let Some(path) = candidates.iter().find(|path| Path::new(path).exists()) {
            env::set_var("SHADERC_LIB_PATH", path);
        }
    }
    let compiler = shaderc_dyn::Compiler::new().expect("load libshaderc_shared");
    let mut options = shaderc_dyn::CompileOptions::new().expect("shaderc options");
    options
        .set_target_env(shaderc_dyn::TargetEnv::Vulkan, shaderc_dyn::EnvVersion::Vulkan1_2 as u32);
    options.set_target_spirv(shaderc_dyn::SpirvVersion::V1_5);
    options.set_include_callback(|name, _, _, _| {
        let path = Path::new("src/render/shaders_common").join(name);
        let content = fs::read_to_string(&path)
            .map_err(|error| format!("include {}: {error}", path.display()))?;
        Ok(shaderc_dyn::ResolvedInclude { resolved_name: name.to_string(), content })
    });
    verify_color_snippet(&compiler);
    let mut generated = String::new();
    for (path, kind) in shaders {
        let source = fs::read_to_string(path).unwrap_or_else(|error| {
            panic!("read {}: {error}", path.display());
        });
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("shader");
        let spirv = compiler
            .compile_into_spirv(&source, *kind, name, "main", Some(&options))
            .unwrap_or_else(|error| panic!("compile {name}: {error}"));
        let spirv_name = format!("{name}.spv");
        fs::write(out_dir.join(&spirv_name), spirv.as_binary_u8())
            .unwrap_or_else(|error| panic!("write {spirv_name}: {error}"));
        let ident = spirv_ident(name);
        generated.push_str(&format!(
            "pub static {ident}: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{spirv_name}\"));\n"
        ));
    }
    generated
}

/// Compiles the shared snippet as Vulkan GLSL 450 and as GLSL ES 1.00.
/// The live texture shaders do not include it; identity draws stay on them.
fn verify_color_snippet(compiler: &shaderc_dyn::Compiler) {
    let vulkan = "\
#version 450
layout(std140, set = 0, binding = 0) uniform ColorParams {
    vec4 u_color_matrix_0;
    vec4 u_color_matrix_1;
    vec4 u_color_matrix_2;
    float u_src_bt1886_lb_over_lw;
    float u_dst_bt1886_lb_over_lw;
    float u_src_power_exponent;
    float u_dst_power_exponent;
    float u_tonemap_max;
    float u_tonemap_dst_max;
    float u_tonemap_dst_min;
    float u_tonemap_dst_ref;
    vec4 u_tonemap_to_lms_0;
    vec4 u_tonemap_to_lms_1;
    vec4 u_tonemap_to_lms_2;
    vec4 u_tonemap_from_lms_0;
    vec4 u_tonemap_from_lms_1;
    vec4 u_tonemap_from_lms_2;
};
#include \"color.glsl\"
layout(location = 0) out vec4 out_color;
void main() {
    vec3 color = decode_tf(2, vec3(0.5));
    color = encode_tf(2, color);
    out_color = apply_color(vec4(color, 1.0), 1, 2, 2, 0, 0, 1, 0);
}
";
    compile_checked(
        compiler,
        vulkan,
        shaderc_dyn::TargetEnv::Vulkan,
        shaderc_dyn::EnvVersion::Vulkan1_2 as u32,
        "color.vulkan.frag",
    );
    verify_es_snippet();
    verify_gles_shaders();
}

/// shaderc emits SPIR-V, which cannot represent `#version 100`.
/// glslangValidator checks the concatenated GLES source as ES 1.00.
fn verify_es_snippet() {
    let snippet =
        fs::read_to_string("src/render/shaders_common/color.glsl").expect("read color.glsl");
    let source = format!(
        "#version 100\nprecision highp float;\n#define COLOR_LOOSE_UNIFORMS 1\n{snippet}\nvoid main() {{\n    vec3 color = decode_tf(2, vec3(0.5));\n    color = encode_tf(2, color);\n    gl_FragColor = apply_color(vec4(color, 1.0), 1, 2, 2, 0, 0, 1, 0);\n}}\n"
    );
    let path = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("color.es.frag");
    fs::write(&path, source).expect("write ES color shader");
    let output = Command::new("glslangValidator")
        .args(["-S", "frag", "--glsl-version", "100"])
        .arg(&path)
        .output()
        .expect("run glslangValidator for the ES 1.00 color snippet");
    if !output.status.success() {
        panic!(
            "compile color.es.frag:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Validates the GLES fragment shaders as GLSL ES 1.00, with the shared
/// snippet pasted in the way `render::shader_source::expand_gles_includes`
/// does it at runtime.
fn verify_gles_shaders() {
    let snippet = fs::read_to_string("src/render/shaders_common/rounded_rect.glsl")
        .expect("read rounded_rect.glsl");
    let color =
        fs::read_to_string("src/render/shaders_common/color.glsl").expect("read color.glsl");
    let expand = |source: &str| source.replace("#include \"rounded_rect.glsl\"", &snippet);
    let read = |path: &str| {
        fs::read_to_string(path).unwrap_or_else(|error| panic!("read {path}: {error}"))
    };
    let texture = read("src/shaders/texture_clip.frag");
    let texture_body = texture
        .strip_prefix("precision highp float;\n")
        .unwrap_or(&texture);
    let color_texture = format!(
        "precision highp float;\n#define SRC_TF 2\n#define DST_TF 2\n#define HAS_MATRIX 0\n#define ALPHA_MODE 0\n#define YUV_MODE 0\n#define TONEMAP 0\n#define NEEDS_COLOR_PATH 1\n#define COLOR_LOOSE_UNIFORMS 1\n{color}{texture_body}"
    );
    let sources = [
        ("texture_clip.es.frag", expand(&texture)),
        ("texture_clip_color.es.frag", expand(&color_texture)),
        ("border.es.frag", expand(&read("src/shaders/border.frag"))),
        ("shadow.es.frag", expand(&read("src/shaders/shadow.frag"))),
    ];
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    for (name, source) in sources {
        let path = out_dir.join(name);
        fs::write(&path, format!("#version 100\n{source}"))
            .unwrap_or_else(|error| panic!("write {name}: {error}"));
        let output = Command::new("glslangValidator")
            .args(["-S", "frag", "--glsl-version", "100"])
            .arg(&path)
            .output()
            .expect("run glslangValidator for the GLES shaders");
        if !output.status.success() {
            panic!(
                "compile {name}:\n{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

fn compile_checked(
    compiler: &shaderc_dyn::Compiler,
    source: &str,
    env: shaderc_dyn::TargetEnv,
    version: u32,
    name: &str,
) {
    let mut options = shaderc_dyn::CompileOptions::new().expect("shaderc options");
    options.set_target_env(env, version);
    if env == shaderc_dyn::TargetEnv::Vulkan {
        options.set_target_spirv(shaderc_dyn::SpirvVersion::V1_5);
    }
    options.set_include_callback(|included, _, _, _| {
        let path = Path::new("src/render/shaders_common").join(included);
        let content = fs::read_to_string(&path)
            .map_err(|error| format!("include {}: {error}", path.display()))?;
        Ok(shaderc_dyn::ResolvedInclude { resolved_name: included.to_string(), content })
    });
    compiler
        .compile_into_spirv(source, shaderc_dyn::ShaderKind::Fragment, name, "main", Some(&options))
        .unwrap_or_else(|error| panic!("compile {name}: {error}"));
}

fn spirv_ident(file_name: &str) -> String {
    let mut ident = String::new();
    for character in file_name.chars() {
        if character.is_ascii_alphanumeric() {
            ident.push(character.to_ascii_uppercase());
        } else if !ident.ends_with('_') {
            ident.push('_');
        }
    }
    ident.trim_matches('_').to_string()
}
