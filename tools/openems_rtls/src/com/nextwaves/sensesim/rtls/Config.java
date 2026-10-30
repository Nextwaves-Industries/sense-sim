package com.nextwaves.sensesim.rtls;

/**
 * Component configuration. OpenEMS resolves this class by the metatype OCD id
 * (see OSGI-INF/metatype/Rtls.SenseSim.xml) to build its property channels.
 */
public @interface Config {
	String id() default "rtls0";

	String alias() default "";

	boolean enabled() default true;
}
