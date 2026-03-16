use rustbox_core::sandbox::Runtime;

/// Map a sandbox runtime to a Docker image name.
///
/// With the default prefix `"rustbox"`, produces local image tags like
/// `rustbox-node24:latest`. If a registry prefix is provided (e.g.
/// `"ghcr.io/myorg/rustbox"`), produces `ghcr.io/myorg/rustbox-node24:latest`.
pub fn image_for_runtime(runtime: &Runtime, prefix: &str) -> String {
    let tag = match runtime {
        Runtime::Node24 => "node24",
        Runtime::Node22 => "node22",
        Runtime::Python313 => "python313",
    };
    format!("{prefix}-{tag}:latest")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_names() {
        assert_eq!(image_for_runtime(&Runtime::Node24, "rustbox"), "rustbox-node24:latest");
        assert_eq!(image_for_runtime(&Runtime::Node22, "rustbox"), "rustbox-node22:latest");
        assert_eq!(image_for_runtime(&Runtime::Python313, "rustbox"), "rustbox-python313:latest");
    }

    #[test]
    fn image_names_with_registry() {
        assert_eq!(
            image_for_runtime(&Runtime::Node24, "ghcr.io/myorg/rustbox"),
            "ghcr.io/myorg/rustbox-node24:latest"
        );
    }
}
