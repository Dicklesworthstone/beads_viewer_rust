/*
 * HybridScorer - Client-side graph-aware search ranking
 * Mirrors pkg/search/hybrid_scorer_impl.go normalization logic.
 */

const HYBRID_PRESETS = {
  default: { text: 0.40, pagerank: 0.20, status: 0.15, impact: 0.10, priority: 0.10, recency: 0.05 },
  'bug-hunting': { text: 0.30, pagerank: 0.15, status: 0.15, impact: 0.15, priority: 0.20, recency: 0.05 },
  'sprint-planning': { text: 0.30, pagerank: 0.20, status: 0.25, impact: 0.15, priority: 0.05, recency: 0.05 },
  'impact-first': { text: 0.25, pagerank: 0.30, status: 0.10, impact: 0.20, priority: 0.10, recency: 0.05 },
  'text-only': { text: 1.00, pagerank: 0.00, status: 0.00, impact: 0.00, priority: 0.00, recency: 0.00 }
};

class HybridScorer {
  constructor(weights = HYBRID_PRESETS.default) {
    this.weights = { ...weights };
    this.maxBlockerCount = 0;
  }

  score(result) {
    const statusScore = this.normalizeStatus(result.status);
    const priorityScore = this.normalizePriority(result.priority);
    const impactScore = this.normalizeImpact(result.blockerCount);
    const recencyScore = this.normalizeRecency(result.updatedAt);
    const pagerank = HybridScorer.unitScore(result.pagerank, 0.5);
    const textScore = HybridScorer.unitScore(result.textScore, 0);

    const finalScore =
      this.weights.text * textScore +
      this.weights.pagerank * pagerank +
      this.weights.status * statusScore +
      this.weights.impact * impactScore +
      this.weights.priority * priorityScore +
      this.weights.recency * recencyScore;

    return {
      ...result,
      hybrid_score: finalScore,
      component_scores: {
        pagerank,
        status: statusScore,
        impact: impactScore,
        priority: priorityScore,
        recency: recencyScore,
      },
    };
  }

  scoreAndRank(results) {
    if (!Array.isArray(results) || results.length === 0) {
      return [];
    }
    const maxBlocker = results.reduce(
      (max, r) => Number.isFinite(r.blockerCount)
        ? Math.max(max, r.blockerCount) : max,
      0
    );
    this.maxBlockerCount = maxBlocker;

    return results
      .map(r => this.score(r))
      .sort(HybridScorer.compareResults);
  }

  static unitScore(value, fallback) {
    return Number.isFinite(value) ? Math.max(0, Math.min(1, value)) : fallback;
  }

  static compareResults(a, b) {
    const scoreOrder = b.hybrid_score - a.hybrid_score;
    if (scoreOrder !== 0) return scoreOrder;
    const left = String(a.id ?? a.issue_id ?? '');
    const right = String(b.id ?? b.issue_id ?? '');
    return left < right ? -1 : left > right ? 1 : 0;
  }

  normalizeStatus(status) {
    const STATUS_WEIGHTS = { open: 1.0, in_progress: 0.8, blocked: 0.5, closed: 0.1 };
    return Object.prototype.hasOwnProperty.call(STATUS_WEIGHTS, status)
      ? STATUS_WEIGHTS[status] : 0.5;
  }

  normalizePriority(priority) {
    const PRIORITY_WEIGHTS = [1.0, 0.8, 0.6, 0.4, 0.2];
    return PRIORITY_WEIGHTS[priority] ?? 0.5;
  }

  normalizeImpact(blockerCount) {
    if (this.maxBlockerCount === 0) return 0.5;
    if (!Number.isFinite(blockerCount)) return 0;
    if (blockerCount <= 0) return 0;
    if (blockerCount >= this.maxBlockerCount) return 1.0;
    return blockerCount / this.maxBlockerCount;
  }

  normalizeRecency(updatedAt) {
    if (!updatedAt) return 0.5;
    const timestamp = new Date(updatedAt).getTime();
    if (!Number.isFinite(timestamp)) return 0.5;
    // Clock skew and valid far-future dates must not turn a normalized
    // component into Infinity (even zero-weight components would yield NaN).
    const daysSince = Math.max(0, (Date.now() - timestamp) / (1000 * 60 * 60 * 24));
    return Math.exp(-daysSince / 30);
  }
}

window.HybridScorer = HybridScorer;
window.HYBRID_PRESETS = HYBRID_PRESETS;
