package dev.mcpack.agent;

import net.minecraft.client.Minecraft;
import net.minecraft.client.gui.screens.GenericMessageScreen;
import net.minecraft.client.gui.screens.TitleScreen;
import net.minecraft.core.registries.Registries;
import net.minecraft.network.chat.Component;
import net.minecraft.world.Difficulty;
import net.minecraft.world.level.GameRules;
import net.minecraft.world.level.GameType;
import net.minecraft.world.level.LevelSettings;
import net.minecraft.world.level.WorldDataConfiguration;
import net.minecraft.world.level.levelgen.WorldOptions;
import net.minecraft.world.level.levelgen.presets.WorldPresets;

public final class VersionOpsImpl implements VersionOps {
    @Override
    public void createWorld(String levelId, String displayName, Long seed, String gameMode, boolean flat, boolean cheats) {
        Minecraft mc = Minecraft.getInstance();
        GameType type = GameType.byName(gameMode, GameType.CREATIVE);
        LevelSettings settings = new LevelSettings(displayName, type, false, Difficulty.NORMAL, cheats,
                new GameRules(), WorldDataConfiguration.DEFAULT);
        WorldOptions options = new WorldOptions(seed != null ? seed : WorldOptions.randomSeed(), !flat, false);
        mc.createWorldOpenFlows().createFreshLevel(levelId, settings, options, registries -> flat
                ? registries.lookupOrThrow(Registries.WORLD_PRESET).getOrThrow(WorldPresets.FLAT).value().createWorldDimensions()
                : WorldPresets.createNormalWorldDimensions(registries), new TitleScreen());
    }

    @Override
    public void joinWorld(String levelId) {
        Minecraft mc = Minecraft.getInstance();
        mc.createWorldOpenFlows().openWorld(levelId, () -> mc.setScreen(new TitleScreen()));
    }

    @Override
    public void leaveWorld() {
        Minecraft mc = Minecraft.getInstance();
        if (mc.level != null) {
            mc.level.disconnect();
        }
        mc.disconnect(new GenericMessageScreen(Component.translatable("menu.savingLevel")));
        mc.setScreen(new TitleScreen());
    }
}
