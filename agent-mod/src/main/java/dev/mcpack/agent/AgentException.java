package dev.mcpack.agent;

/** An error reported to the launcher as the call's error message. */
final class AgentException extends RuntimeException {
    AgentException(String message) {
        super(message);
    }
}
