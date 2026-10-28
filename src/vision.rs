//! Local FAST/BRIEF correspondence and robust affine point tracking, entirely in Rust.
use image::{GrayImage, ImageFormat};
use imageproc::{binary_descriptors::{BinaryDescriptor, brief::{brief, TestPair}}, corners::corners_fast9, point::Point};
use nalgebra::{Matrix3, Vector3};
use rand::{Rng, SeedableRng, rngs::StdRng};
use std::{collections::HashSet, sync::OnceLock};

fn pairs() -> &'static Vec<TestPair> {
    static PAIRS: OnceLock<Vec<TestPair>>=OnceLock::new();
    PAIRS.get_or_init(|| {
        let mut rng=StdRng::seed_from_u64(20261028);
        (0..256).map(|_|TestPair { p0:Point::new(rng.random_range(0..31),rng.random_range(0..31)),p1:Point::new(rng.random_range(0..31),rng.random_range(0..31)) }).collect()
    })
}
fn features(image: &GrayImage) -> Vec<imageproc::binary_descriptors::brief::BriefDescriptor> {
    let (w,h)=image.dimensions();
    let mut corners=corners_fast9(image,12);
    corners.retain(|p|p.x>=24 && p.y>=24 && p.x+24<w && p.y+24<h && p.y>h*7/100 && !(p.y>h*88/100 && p.x<w/5));
    corners.sort_unstable_by(|a,b|b.score.total_cmp(&a.score));
    let mut cells=HashSet::new();
    let points:Vec<_>=corners.into_iter().filter(|p|cells.insert((p.x/5,p.y/5))).take(1600).map(|p|Point::new(p.x,p.y)).collect();
    brief(image,&points,256,Some(pairs())).map(|(d,_)|d).unwrap_or_default()
}
#[derive(Clone,Copy)]
struct Match { a:[f64;2], b:[f64;2] }
fn affine(matches:&[Match],indices:&[usize]) -> Option<(Vector3<f64>,Vector3<f64>)> {
    let mut normal=Matrix3::zeros();let mut bx=Vector3::zeros();let mut by=Vector3::zeros();
    for &i in indices { let m=matches[i];let v=Vector3::new(m.a[0],m.a[1],1.);normal+=v*v.transpose();bx+=v*m.b[0];by+=v*m.b[1]; }
    let inverse=normal.try_inverse()?;Some((inverse*bx,inverse*by))
}
fn apply(model:&(Vector3<f64>,Vector3<f64>),point:[f64;2])->[f64;2] { let v=Vector3::new(point[0],point[1],1.);[model.0.dot(&v),model.1.dot(&v)] }
fn distance(a:[f64;2],b:[f64;2])->f64 { (a[0]-b[0]).hypot(a[1]-b[1]) }

pub fn track(before:&[u8],after:&[u8],point:[f64;2])->Option<[f64;2]> {
    let decode=|data|image::load_from_memory_with_format(data,ImageFormat::Jpeg).ok().map(|i|i.thumbnail(640,480).to_luma8());
    let a=decode(before)?;let b=decode(after)?;
    if a.dimensions()!=b.dimensions() { return None; }
    let (w,h)=(a.width() as f64,a.height() as f64);
    let pixel=[point[0]*w,point[1]*h];
    let da=features(&a);let db=features(&b);
    let mut matches=Vec::new();let mut used=HashSet::new();
    for descriptor in &da {
        let source=[descriptor.corner.x as f64,descriptor.corner.y as f64];
        if distance([source[0]/w,source[1]/h],point)>0.25 { continue; }
        let mut best=(u32::MAX,0);let mut second=u32::MAX;
        for (index,candidate) in db.iter().enumerate() {
            let d=descriptor.hamming_distance(candidate);
            if d<best.0 { second=best.0;best=(d,index); } else if d<second { second=d; }
        }
        if best.0<75 && (best.0 as f64)<0.72*(second as f64) && used.insert(best.1) {
            let target=&db[best.1].corner;
            matches.push(Match{a:source,b:[target.x as f64,target.y as f64]});
        }
    }
    if matches.len()<12 { return None; }
    let mut rng=StdRng::seed_from_u64(42);let mut inliers=Vec::new();
    for _ in 0..250 {
        let indices=[rng.random_range(0..matches.len()),rng.random_range(0..matches.len()),rng.random_range(0..matches.len())];
        if let Some(model)=affine(&matches,&indices) {
            let candidates:Vec<_>=matches.iter().enumerate().filter(|(_,m)|distance(apply(&model,m.a),m.b)<3.).map(|(i,_)|i).collect();
            if candidates.len()>inliers.len() { inliers=candidates; }
        }
    }
    if inliers.len()<10 || (inliers.len() as f64)<matches.len() as f64*0.4 { return None; }
    if !inliers.iter().any(|&i| distance([matches[i].a[0]/w,matches[i].a[1]/h],point)<0.12) { return None; }
    let model=affine(&matches,&inliers)?;
    let sx=model.0[0].hypot(model.1[0]);let sy=model.0[1].hypot(model.1[1]);
    if !(0.4..=2.5).contains(&sx) || !(0.4..=2.5).contains(&sy) { return None; }
    let result=apply(&model,pixel);let result=[result[0]/w,result[1]/h];
    result.iter().all(|v|v.is_finite() && (0.0..=1.0).contains(v)).then_some(result)
}

#[cfg(test)] mod tests {
    use super::*;
    use image::{Luma,codecs::jpeg::JpegEncoder};
    fn jpeg(img:&GrayImage)->Vec<u8> { let mut out=vec![];JpegEncoder::new_with_quality(&mut out,90).encode_image(img).unwrap();out }
    #[test] fn follows_translation() {
        let mut rng=StdRng::seed_from_u64(1);let mut a=GrayImage::from_pixel(640,360,Luma([128]));
        for _ in 0..1500 { let x=rng.random_range(20..600);let y=rng.random_range(20..320);let value=rng.random_range(0..255);for xx in x..x+4 {for yy in y..y+4 {a.put_pixel(xx,yy,Luma([value]));}} }
        let mut b=GrayImage::from_pixel(640,360,Luma([128]));for y in 0..340 {for x in 0..610 {b.put_pixel(x+30,y+20,*a.get_pixel(x,y));}}
        let p=track(&jpeg(&a),&jpeg(&b),[0.5,0.5]).unwrap();
        assert!(distance(p,[0.5+30./640.,0.5+20./360.])<0.01,"{p:?}");
    }
    #[test] fn no_claim_for_blank_image() { let img=jpeg(&GrayImage::new(640,360));assert!(track(&img,&img,[0.5,0.5]).is_none()); }
}
