use crate::{
    conv,
    device::MAX_MIP_LEVELS,
    resource::TextureUsage,
    TextureId,
};
use super::{range::RangedStates, PendingTransition, ResourceState, Stitch, Unit};

use arrayvec::ArrayVec;

use std::ops::Range;


type PlaneStates<T> = RangedStates<hal::image::Layer, T>;

//TODO: store `hal::image::State` here to avoid extra conversions
#[derive(Clone, Copy, Debug, PartialEq)]
struct DepthStencilState {
    depth: Unit<TextureUsage>,
    stencil: Unit<TextureUsage>,
}

#[derive(Clone, Debug, Default)]
pub struct TextureStates {
    color_mips: ArrayVec<[PlaneStates<Unit<TextureUsage>>; MAX_MIP_LEVELS]>,
    depth_stencil: PlaneStates<DepthStencilState>,
}

impl PendingTransition<TextureStates> {
    pub fn to_states(&self) -> Range<hal::image::State> {
        conv::map_texture_state(self.usage.start, self.selector.aspects) ..
        conv::map_texture_state(self.usage.end, self.selector.aspects)
    }
}

impl ResourceState for TextureStates {
    type Id = TextureId;
    type Selector = hal::image::SubresourceRange;
    type Usage = TextureUsage;

    fn query(
        &self,
        selector: Self::Selector,
    ) -> Option<Self::Usage> {
        let mut usage = None;
        if selector.aspects.contains(hal::format::Aspects::COLOR) {
            let num_levels = self.color_mips.len();
            let layer_start = num_levels.min(selector.levels.start as usize);
            let layer_end = num_levels.min(selector.levels.end as usize);
            for layer in self.color_mips[layer_start .. layer_end].iter() {
                for &(ref range, ref unit) in layer.iter() {
                    if range.end > selector.layers.start && range.start < selector.layers.end {
                        let old = usage.replace(unit.last);
                        if old.is_some() && old != usage {
                            return None
                        }
                    }
                }
            }
        }
        if selector.aspects.intersects(hal::format::Aspects::DEPTH | hal::format::Aspects::STENCIL) {
            for &(ref range, ref ds) in self.depth_stencil.iter() {
                if range.end > selector.layers.start && range.start < selector.layers.end {
                    if selector.aspects.contains(hal::format::Aspects::DEPTH) {
                        let old = usage.replace(ds.depth.last);
                        if old.is_some() && old != usage {
                            return None
                        }
                    }
                    if selector.aspects.contains(hal::format::Aspects::STENCIL) {
                        let old = usage.replace(ds.stencil.last);
                        if old.is_some() && old != usage {
                            return None
                        }
                    }
                }
            }
        }
        usage
    }

    fn change(
        &mut self,
        id: Self::Id,
        selector: Self::Selector,
        usage: Self::Usage,
        mut output: Option<&mut Vec<PendingTransition<Self>>>,
    ) -> Result<(), PendingTransition<Self>> {
        if selector.aspects.contains(hal::format::Aspects::COLOR) {
            while self.color_mips.len() < selector.levels.end as usize {
                self.color_mips.push(PlaneStates::default());
            }
            for level in selector.levels.clone() {
                let layers = self
                    .color_mips[level as usize]
                    .isolate(&selector.layers, Unit::new(usage));
                for &mut (ref range, ref mut unit) in layers {
                    let old = unit.last;
                    if old == usage {
                        continue
                    }
                    let pending = PendingTransition {
                        id,
                        selector: hal::image::SubresourceRange {
                            aspects: hal::format::Aspects::COLOR,
                            levels: level .. level + 1,
                            layers: range.clone(),
                        },
                        usage: old .. usage,
                    };
                    unit.last = match output.as_mut() {
                        Some(out) => {
                            out.push(pending);
                            usage
                        }
                        None => {
                            if !old.is_empty() && TextureUsage::WRITE_ALL.intersects(old | usage) {
                                return Err(pending);
                            }
                            old | usage
                        }
                    };
                }
            }
        }
        if selector.aspects.intersects(hal::format::Aspects::DEPTH | hal::format::Aspects::STENCIL) {
            for level in selector.levels.clone() {
                let ds_state = DepthStencilState {
                    depth: Unit::new(usage),
                    stencil: Unit::new(usage),
                };
                for &mut (ref range, ref mut unit) in self.depth_stencil
                    .isolate(&selector.layers, ds_state)
                {
                    //TODO: check if anything needs to be done when only one of the depth/stencil
                    // is selected?
                    if unit.depth.last != usage && selector.aspects.contains(hal::format::Aspects::DEPTH) {
                        let old = unit.depth.last;
                        let pending = PendingTransition {
                            id,
                            selector: hal::image::SubresourceRange {
                                aspects: hal::format::Aspects::DEPTH,
                                levels: level .. level + 1,
                                layers: range.clone(),
                            },
                            usage: old .. usage,
                        };
                        unit.depth.last = match output.as_mut() {
                            Some(out) => {
                                out.push(pending);
                                usage
                            }
                            None => {
                                if !old.is_empty() && TextureUsage::WRITE_ALL.intersects(old | usage) {
                                    return Err(pending);
                                }
                                old | usage
                            }
                        };
                    }
                    if unit.stencil.last != usage && selector.aspects.contains(hal::format::Aspects::STENCIL) {
                        let old = unit.stencil.last;
                        let pending = PendingTransition {
                            id,
                            selector: hal::image::SubresourceRange {
                                aspects: hal::format::Aspects::STENCIL,
                                levels: level .. level + 1,
                                layers: range.clone(),
                            },
                            usage: old .. usage,
                        };
                        unit.stencil.last = match output.as_mut() {
                            Some(out) => {
                                out.push(pending);
                                usage
                            }
                            None => {
                                if !old.is_empty() && TextureUsage::WRITE_ALL.intersects(old | usage) {
                                    return Err(pending);
                                }
                                old | usage
                            }
                        };
                    }
                }
            }
        }
        Ok(())
    }

    fn merge(
        &mut self,
        id: Self::Id,
        other: &Self,
        stitch: Stitch,
        mut output: Option<&mut Vec<PendingTransition<Self>>>,
    ) -> Result<(), PendingTransition<Self>> {
        let mut temp_color = Vec::new();
        while self.color_mips.len() < other.color_mips.len() {
            self.color_mips.push(PlaneStates::default());
        }
        for (mip_id, (mip_self, mip_other)) in self.color_mips
            .iter_mut()
            .zip(&other.color_mips)
            .enumerate()
        {
            temp_color.extend(mip_self.merge(mip_other, 0));
            mip_self.clear();
            for (layers, states) in temp_color.drain(..) {
                let color_usage = states.start.last .. states.end.select(stitch);
                if let Some(out) = output.as_mut() {
                    if color_usage.start != color_usage.end {
                        let level = mip_id as hal::image::Level;
                        out.push(PendingTransition {
                            id,
                            selector: hal::image::SubresourceRange {
                                aspects: hal::format::Aspects::COLOR,
                                levels: level .. level + 1,
                                layers: layers.clone(),
                            },
                            usage: color_usage.clone(),
                        });
                    }
                }
                mip_self.append(layers, Unit {
                    init: states.start.init,
                    last: color_usage.end,
                });
            }
        }

        let mut temp_ds = Vec::new();
        temp_ds.extend(self.depth_stencil.merge(&other.depth_stencil, 0));
        self.depth_stencil.clear();
        for (layers, states) in temp_ds.drain(..) {
            let usage_depth = states.start.depth.last .. states.end.depth.select(stitch);
            let usage_stencil = states.start.stencil.last .. states.end.stencil.select(stitch);
            if let Some(out) = output.as_mut() {
                if usage_depth.start != usage_depth.end {
                    out.push(PendingTransition {
                        id,
                        selector: hal::image::SubresourceRange {
                            aspects: hal::format::Aspects::DEPTH,
                            levels: 0 .. 1,
                            layers: layers.clone(),
                        },
                        usage: usage_depth.clone(),
                    });
                }
                if usage_stencil.start != usage_stencil.end {
                    out.push(PendingTransition {
                        id,
                        selector: hal::image::SubresourceRange {
                            aspects: hal::format::Aspects::STENCIL,
                            levels: 0 .. 1,
                            layers: layers.clone(),
                        },
                        usage: usage_stencil.clone(),
                    });
                }
            }
            self.depth_stencil.append(layers, DepthStencilState {
                depth: Unit {
                    init: states.start.depth.init,
                    last: usage_depth.end,
                },
                stencil: Unit {
                    init: states.start.stencil.init,
                    last: usage_stencil.end,
                },
            });
        }

        Ok(())
    }
}
