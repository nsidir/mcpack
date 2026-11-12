package dev.mcpack.agent;

/** Operations whose Minecraft API differs between versions. Implemented per version as VersionOpsImpl. Called on the render thread. */
public interface VersionOps {
    /** Create a new singleplayer world in folder {@code levelId} and join it. */
    void createWorld(String levelId, String displayName, Long seed, String gameMode, boolean flat, boolean cheats);

    /** Join an existing singleplayer world by folder name. */
    void joinWorld(String levelId);

    /** Save and leave the current world, returning to the title screen. */
    void leaveWorld();
}
