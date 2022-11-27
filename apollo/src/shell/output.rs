use crate::utils::geometry::{Rectangle, Logical};

#[derive(Debug)]
pub struct Output {
    geometry: Rectangle<i32, Logical>,
    name: String,
    global_id: u32,
}

#[derive(Debug)]
pub struct Screen {
    outputs: Vec<Output>,
}
