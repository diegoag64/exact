//! The quality ladder (LLP 1139.003 D3): the rung a world's renderer is made
//! at, from the game's `Game::quality` and the device's GPU tier, and what
//! each rung clamps of what the game left unsaid. Presentation only: nothing
//! here reaches the world, its saves or its hash.

use crate::WaterEffects;
use exact_game::{Quality, SceneSamples, AUTO_RENDER_SCALE};
use exact_gpu::GpuTier;

/// A resolved rung of the ladder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rung {
    /// What the game asked for.
    pub asked: Quality,
    /// The device's tier when the renderer was made.
    pub tier: GpuTier,
    /// The rung itself: `Auto` resolved by the tier.
    pub level: GpuTier,
    /// The immersive profile (`crate::profile`) replaces the rung's clamps:
    /// asked for with `Quality::Xr`, and taken by every immersive frame.
    pub xr: bool,
}

impl Rung {
    /// `asked` under `tier`: `Auto` takes the tier's rung.
    pub fn resolve(asked: Quality, tier: GpuTier) -> Self {
        let level = match asked {
            Quality::Auto | Quality::Xr => tier,
            Quality::Low => GpuTier::Low,
            Quality::Mid => GpuTier::Mid,
            Quality::High => GpuTier::High,
        };
        Self {
            asked,
            tier,
            level,
            xr: asked == Quality::Xr,
        }
    }

    /// This rung for an immersive frame: the profile's.
    pub fn immersive(self) -> Self {
        Self { xr: true, ..self }
    }

    /// Scene samples: one under `Low`; the game's own as the ceiling.
    pub fn samples(self, game: SceneSamples) -> SceneSamples {
        // The immersive profile keeps the game's sample request.
        if self.xr {
            // `msaa-1`: a measuring switch (docs/quest.md), never a setting.
            if exact_gpu::debug_switch("msaa-1") {
                return SceneSamples::One;
            }
            return game;
        }
        if self.level == GpuTier::Low {
            SceneSamples::One
        } else {
            game
        }
    }

    /// The world raster's scale: the rung's when `render_scale` keeps its
    /// default, [`AUTO_RENDER_SCALE`]; any other value the game returns,
    /// unchanged.
    pub fn scale(self, game: f32) -> f32 {
        if game != AUTO_RENDER_SCALE {
            return game;
        }
        if self.xr {
            return 1.0;
        }
        match self.level {
            GpuTier::Low => 0.5,
            GpuTier::Mid => 0.75,
            GpuTier::High => 1.0,
        }
    }

    /// Clamp one frame's input. `authored_shadows`: the game inserted a
    /// `Shadows` resource, which is its own and stands.
    pub fn clamp(self, input: &mut crate::FrameInput<'_>, authored_shadows: bool) {
        self.clamp_staged(input, authored_shadows, None);
    }

    /// `clamp`, with where the game places its world in a headset, which
    /// the profile fits the sun's shadow to.
    pub fn clamp_staged(
        self,
        input: &mut crate::FrameInput<'_>,
        authored_shadows: bool,
        stage: Option<&exact_game::XrStage>,
    ) {
        if self.xr {
            return crate::profile::clamp(input, authored_shadows, stage);
        }
        if self.level == GpuTier::High {
            return;
        }
        if !authored_shadows {
            if let Some(sun) = &mut input.sun {
                match self.level {
                    GpuTier::Low => sun.shadows = None,
                    _ => {
                        if let Some(shadows) = &mut sun.shadows {
                            shadows.cascades = shadows.cascades.min(1);
                        }
                    }
                }
            }
        }
        // Local layers are memory a game's `Shadows` cannot know the device
        // has: Low keeps today's 1024² (4 MiB a layer), Mid at most 2048².
        let cap = if self.level == GpuTier::Low {
            1024
        } else {
            2048
        };
        let local = &mut input.local_shadows.resolution;
        *local = (*local).min(cap);
        // An inserted `AmbientOcclusion` is the game's, quality and all.
    }

    /// Which of a `Water` resource's effects the renderer draws: none under
    /// `Low`, both elsewhere. A present-time choice (`Renderer::set_water_effects`):
    /// the saved resource keeps the game's caustics and shafts.
    pub fn water_effects(self) -> WaterEffects {
        if self.xr {
            return crate::profile::water_effects();
        }
        let on = self.level != GpuTier::Low;
        WaterEffects {
            caustics: on,
            shafts: on,
        }
    }

    fn json(self, samples: u32, scale: f32) -> String {
        let asked = match self.asked {
            Quality::Auto => "auto",
            Quality::Low => "low",
            Quality::Mid => "mid",
            Quality::High => "high",
            Quality::Xr => "xr",
        };
        format!(
            r#"{{"asked":"{asked}","tier":"{}","rung":"{}","samples":{},"scale":{scale}}}"#,
            self.tier.keyword(),
            if self.xr { "xr" } else { self.level.keyword() },
            samples
        )
    }
}

/// The raster's scale before a binding resolved a rung: the game's, full
/// size by default.
pub(crate) fn unresolved_scale(game: f32) -> f32 {
    if game == AUTO_RENDER_SCALE {
        1.0
    } else {
        game
    }
}

/// A renderer's scene samples: the rung's, under the game's; the game's
/// before a binding resolved the rung.
pub(crate) fn samples(rung: Option<Rung>, game: SceneSamples) -> u32 {
    let wanted = rung.map_or(game, |rung| rung.samples(game));
    #[cfg(target_os = "android")]
    if rung.is_some_and(|r| r.xr)
        && exact_gpu::vulkan::stereo_msaa_two()
        && (wanted == SceneSamples::Two || exact_gpu::debug_switch("msaa-2"))
    {
        return wanted.count().min(2);
    }
    match wanted {
        SceneSamples::Two => SceneSamples::Four.count(),
        _ => wanted.count(),
    }
}

/// The world state's `quality` row, once a binding resolved the rung.
pub(crate) fn row(rung: Option<Rung>, game: SceneSamples, scale: f32, reply: &mut String) {
    if let Some(rung) = rung {
        reply.push_str(",\"quality\":");
        reply.push_str(&rung.json(samples(Some(rung), game), rung.scale(scale)));
    }
}

#[cfg(test)]
mod tests {
    use super::Rung;
    use exact_game::{Quality, SceneSamples, AUTO_RENDER_SCALE};
    use exact_gpu::GpuTier;

    #[test]
    fn auto_follows_the_tier_and_a_named_rung_overrides_it() {
        assert_eq!(
            Rung::resolve(Quality::Auto, GpuTier::Low).level,
            GpuTier::Low
        );
        assert_eq!(
            Rung::resolve(Quality::Auto, GpuTier::High).level,
            GpuTier::High
        );
        assert_eq!(
            Rung::resolve(Quality::High, GpuTier::Low).level,
            GpuTier::High
        );
        assert_eq!(
            Rung::resolve(Quality::Low, GpuTier::High).level,
            GpuTier::Low
        );
    }

    #[test]
    fn a_rung_clamps_only_what_the_game_left_unsaid() {
        let low = Rung::resolve(Quality::Low, GpuTier::High);
        let mid = Rung::resolve(Quality::Mid, GpuTier::High);
        let high = Rung::resolve(Quality::High, GpuTier::Low);
        assert_eq!(low.samples(SceneSamples::Four), SceneSamples::One);
        assert_eq!(mid.samples(SceneSamples::Four), SceneSamples::Four);
        assert_eq!(super::samples(Some(low), SceneSamples::Two), 1);
        assert_eq!(super::samples(Some(mid), SceneSamples::Two), 4);
        assert_eq!(
            mid.samples(SceneSamples::One),
            SceneSamples::One,
            "the game's samples are the ceiling"
        );
        assert_eq!(low.scale(AUTO_RENDER_SCALE), 0.5);
        assert_eq!(mid.scale(AUTO_RENDER_SCALE), 0.75);
        assert_eq!(high.scale(AUTO_RENDER_SCALE), 1.0);
        assert_eq!(low.scale(0.8), 0.8, "a game's own render_scale stands");
        assert_eq!(low.scale(1.0), 1.0, "an explicit full size stands too");
    }

    #[test]
    fn a_clamp_leaves_the_games_own_ambient_occlusion_and_shadows() {
        use exact_game::{AmbientOcclusion, AoQuality, Shadows};
        let sun = || crate::Sun {
            shadows: Some(Shadows::default()),
            ..Default::default()
        };
        let low = Rung::resolve(Quality::Low, GpuTier::High);
        let mut input = crate::FrameInput {
            sun: Some(sun()),
            ambient_occlusion: Some(AmbientOcclusion {
                quality: AoQuality::High,
                ..Default::default()
            }),
            ..Default::default()
        };
        low.clamp(&mut input, false);
        assert!(
            input.sun.unwrap().shadows.is_none(),
            "an unsaid sun shadow goes"
        );
        assert_eq!(input.ambient_occlusion.unwrap().quality, AoQuality::High);
        let mut kept = crate::FrameInput {
            sun: Some(sun()),
            ..Default::default()
        };
        low.clamp(&mut kept, true);
        assert!(
            kept.sun.unwrap().shadows.is_some(),
            "an inserted Shadows stands"
        );
        let mut mid = crate::FrameInput {
            sun: Some(sun()),
            ..Default::default()
        };
        Rung::resolve(Quality::Mid, GpuTier::High).clamp(&mut mid, false);
        assert_eq!(mid.sun.unwrap().shadows.unwrap().cascades, 1);
    }

    #[test]
    fn a_rung_caps_local_shadow_layers_even_when_the_game_asked() {
        let local = |level, resolution| {
            let mut input = crate::FrameInput {
                local_shadows: crate::LocalShadows {
                    resolution,
                    ..Default::default()
                },
                ..Default::default()
            };
            Rung::resolve(level, GpuTier::High).clamp(&mut input, true);
            input.local_shadows.resolution
        };
        assert_eq!(local(Quality::Low, 4096), 1024);
        assert_eq!(local(Quality::Low, 512), 512);
        assert_eq!(local(Quality::Mid, 4096), 2048);
        assert_eq!(local(Quality::High, 4096), 4096);
    }
}
