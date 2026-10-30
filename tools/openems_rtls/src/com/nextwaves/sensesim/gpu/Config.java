package com.nextwaves.sensesim.gpu;

/**
 * Component configuration. OpenEMS resolves this class by the metatype OCD id
 * (see OSGI-INF/metatype/Gpu.SenseSim.xml) to build its property channels.
 */
public @interface Config {
	String id() default "gpu0";

	String alias() default "";

	boolean enabled() default true;
}
