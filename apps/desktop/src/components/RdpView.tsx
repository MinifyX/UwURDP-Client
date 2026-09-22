import { useEffect, useRef } from 'react';
import { RdpDriver, type Fit } from '../lib/rdp';

type Props = {
  fit: Fit;
  onReady: (driver: RdpDriver) => void;
  onDispose: (driver: RdpDriver) => void;
};

/**
 * The place a remote desktop is drawn. The driver lives outside React: this
 * only creates it once, hands it up and ends it when the tab goes away.
 */
export function RdpView({ fit, onReady, onDispose }: Props) {
  const ref = useRef<HTMLDivElement>(null);
  const driverRef = useRef<RdpDriver | null>(null);

  useEffect(() => {
    const container = ref.current;
    if (!container) return;
    const driver = new RdpDriver(container, fit);
    driverRef.current = driver;
    onReady(driver);
    return () => {
      onDispose(driver);
      driver.dispose();
      driverRef.current = null;
    };
    // The driver is made once per mount; fit changes go through setFit.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    driverRef.current?.setFit(fit);
  }, [fit.follow, fit.smartSizing]); // eslint-disable-line react-hooks/exhaustive-deps

  return <div ref={ref} className="rdp-view" />;
}
