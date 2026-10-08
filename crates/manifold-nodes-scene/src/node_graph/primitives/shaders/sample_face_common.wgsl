// Integer arithmetic avoids overflow even on very large source meshes.
fn source_face_index(sample: u32, source_count: u32, sample_count: u32) -> u32 {
    return sample * (source_count / sample_count) + sample * (source_count % sample_count) / sample_count;
}
