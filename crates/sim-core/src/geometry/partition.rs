//! A body's surface, divided into its faces. Every path to a solver starts here: this is what ties
//! a face the user picked to the triangles and elements it covers.
//!
//! From a CAD program it can come as a 3MF with face tags: a welded,
//! watertight mesh in which every triangle names its B-rep face. STEP face order can't be used for
//! this, because export merges and rewrites faces.

use std::io::Read;

use anyhow::{Context, Result, anyhow, bail};
use quick_xml::events::{BytesStart, Event};

/// A body's surface. Positions are in metres.
#[derive(Debug, Clone, Default)]
pub struct Partition {
    pub name: String,
    pub positions: Vec<[f64; 3]>,
    pub triangles: Vec<[u32; 3]>,
    /// The face of each triangle.
    pub tri_face: Vec<u32>,
    /// How many faces the body has.
    pub faces: u32,
}

impl Partition {
    pub fn triangle(&self, i: usize) -> [[f64; 3]; 3] {
        let t = self.triangles[i];
        [self.positions[t[0] as usize], self.positions[t[1] as usize], self.positions[t[2] as usize]]
    }

    /// Each face's area.
    pub fn face_areas(&self) -> Vec<f64> {
        let mut areas = vec![0.0; self.faces as usize];
        for (i, f) in self.tri_face.iter().enumerate() {
            let [a, b, c] = self.triangle(i);
            if let Some(slot) = areas.get_mut(*f as usize) {
                *slot += tri_area(a, b, c);
            }
        }
        areas
    }

    /// Is every edge shared by exactly two triangles, in opposite directions?
    pub fn is_closed(&self) -> bool {
        let mut edges = std::collections::HashMap::with_capacity(self.triangles.len() * 3);
        for t in &self.triangles {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                *edges.entry((a, b)).or_insert(0i32) += 1;
                *edges.entry((b, a)).or_insert(0i32) -= 1;
            }
        }
        edges.values().all(|n| *n == 0)
    }
}

pub fn tri_area(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> f64 {
    let n = cross(sub(b, a), sub(c, a));
    0.5 * dot(n, n).sqrt()
}

pub fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn attr(e: &BytesStart, name: &[u8]) -> Result<Option<String>> {
    for a in e.attributes() {
        let a = a.map_err(|err| anyhow!("bad XML attribute: {err}"))?;
        if a.key.as_ref() == name {
            return Ok(Some(String::from_utf8_lossy(&a.value).into_owned()));
        }
    }
    Ok(None)
}

fn number<T: std::str::FromStr>(e: &BytesStart, name: &[u8]) -> Result<T> {
    let text = attr(e, name)?.ok_or_else(|| anyhow!("<{}> has no {}", String::from_utf8_lossy(e.name().as_ref()), String::from_utf8_lossy(name)))?;
    text.parse().map_err(|_| anyhow!("bad number {text:?} for {}", String::from_utf8_lossy(name)))
}

/// Reads a tagged 3MF package: one [`Partition`] per mesh object. Refuses a file with untagged
/// triangles: a plain 3MF has no faces to hold a boundary condition.
pub fn read_tagged_3mf(bytes: &[u8]) -> Result<Vec<Partition>> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("not a 3MF package")?;
    let mut xml = String::new();
    zip.by_name("3D/3dmodel.model").context("the 3MF package has no 3D model")?.read_to_string(&mut xml).context("reading the 3MF model")?;
    parse_model(&xml)
}

fn parse_model(xml: &str) -> Result<Vec<Partition>> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = Vec::new();
    let mut scale = 1.0;
    let mut current: Option<Partition> = None;
    loop {
        let event = reader.read_event().context("reading the 3MF model")?;
        let (e, ends) = match &event {
            Event::Start(e) => (e, false),
            Event::Empty(e) => (e, true),
            Event::End(e) if e.name().as_ref() == b"object" => {
                if let Some(p) = current.take()
                    && !p.triangles.is_empty()
                {
                    out.push(p);
                }
                continue;
            }
            Event::Eof => break,
            _ => continue,
        };
        match e.name().as_ref() {
            b"model" => {
                scale = match attr(e, b"unit")?.as_deref().unwrap_or("millimeter") {
                    "micron" => 1e-6,
                    "millimeter" => 1e-3,
                    "centimeter" => 1e-2,
                    "inch" => 0.0254,
                    "foot" => 0.3048,
                    "meter" => 1.0,
                    other => bail!("unknown 3MF unit {other:?}"),
                };
            }
            b"object" if !ends => {
                current = Some(Partition { name: attr(e, b"name")?.unwrap_or_default(), faces: attr(e, b"ft:faces")?.and_then(|v| v.parse().ok()).unwrap_or(0), ..Default::default() });
            }
            b"vertex" => {
                if let Some(p) = current.as_mut() {
                    p.positions.push([number::<f64>(e, b"x")? * scale, number::<f64>(e, b"y")? * scale, number::<f64>(e, b"z")? * scale]);
                }
            }
            b"triangle" => {
                if let Some(p) = current.as_mut() {
                    let t = [number::<u32>(e, b"v1")?, number::<u32>(e, b"v2")?, number::<u32>(e, b"v3")?];
                    let face = number::<u32>(e, b"ft:face").context("a triangle has no face tag: this is a plain 3MF, not one with face tags")?;
                    p.triangles.push(t);
                    p.tri_face.push(face);
                    p.faces = p.faces.max(face + 1);
                }
            }
            _ => {}
        }
    }
    for p in &out {
        let n = p.positions.len() as u32;
        if p.triangles.iter().flatten().any(|v| *v >= n) {
            bail!("{}: a triangle refers to a vertex that does not exist", p.name);
        }
    }
    if out.is_empty() {
        bail!("the 3MF has no meshes");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tetrahedron with two faces: three triangles on face 0, one on face 1.
    const MODEL: &str = r#"<?xml version="1.0"?>
<model unit="millimeter" xmlns:ft="https://github.com/bherbruck/solidcraft/formats/3mf-face-tags/2026/10">
 <resources><object id="2" type="model" name="Body1" ft:faces="2"><mesh>
  <vertices><vertex x="0" y="0" z="0"/><vertex x="1000" y="0" z="0"/><vertex x="0" y="1000" z="0"/><vertex x="0" y="0" z="1000"/></vertices>
  <triangles><triangle v1="0" v2="2" v3="1" ft:face="1"/><triangle v1="0" v2="1" v3="3" ft:face="0"/><triangle v1="1" v2="2" v3="3" ft:face="0"/><triangle v1="2" v2="0" v3="3" ft:face="0"/></triangles>
 </mesh></object></resources>
</model>"#;

    #[test]
    fn reads_units_faces_and_closure() {
        let p = parse_model(MODEL).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].name, "Body1");
        assert_eq!(p[0].faces, 2);
        assert_eq!(p[0].positions[1], [1.0, 0.0, 0.0], "millimetres become metres");
        assert_eq!(p[0].tri_face, [1, 0, 0, 0]);
        assert!(p[0].is_closed());
        let a = p[0].face_areas();
        assert!((a[1] - 0.5).abs() < 1e-12, "{a:?}");
    }

    #[test]
    fn an_open_surface_is_not_closed() {
        let mut p = parse_model(MODEL).unwrap().remove(0);
        p.triangles.pop();
        assert!(!p.is_closed());
    }

    #[test]
    fn untagged_and_broken_files_are_refused() {
        let plain = MODEL.replace(" ft:face=\"0\"", "").replace(" ft:face=\"1\"", "");
        assert!(format!("{:#}", parse_model(&plain).unwrap_err()).contains("plain 3MF"));
        let dangling = MODEL.replace("v3=\"3\"", "v3=\"99\"");
        assert!(parse_model(&dangling).unwrap_err().to_string().contains("does not exist"));
        assert!(read_tagged_3mf(b"not a zip").is_err());
    }
}
