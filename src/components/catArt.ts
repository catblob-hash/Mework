/**
 * The mascot, as path data: the cat lying chin-down beside its laptop. The desk it lies on
 * is whatever surface draws it — on the composer, the composer's own top border; beside a
 * streaming round, a ledge drawn for it.
 *
 * It is drawn in two places — loafing on a draft composer's rim, and at its laptop (or
 * pointedly not) beside a streaming round — and nowhere official. The brand mark is a
 * different drawing (`brandMark.ts`), and the two never share a surface. Both mascot surfaces
 * draw this one drawing in the same scene, because redrawing it twice is how two different
 * cats happen.
 *
 * All coordinates are in the artwork's own drawing space.
 *
 * ## Why the face is punched, not painted
 *
 * The face is counter-contours under `fill-rule="evenodd"`, cut out of whatever silhouette
 * carries it. Even-odd only *clears* a subpath lying inside the outer contour, though; one
 * that crossed the cheek line would be filled instead, and a whisker would render as a spike
 * growing out of the silhouette. `catArt.test.ts` checks the face stays inside the body the
 * composer cat punches it out of.
 *
 * ## How the streaming cat moves a drawing that is one piece
 *
 * The head, the haunch and both forelegs are one contour with the face punched out of it, so
 * nothing in it can move until it is freed, and each moving part is freed differently:
 *
 * - Both forepaws are cut out of the body, each along a line its own movement cannot open
 *   ({@link CAT_BODY_WITHOUT_PAWS}).
 * - The pupils and the lids are not cut out at all. They are drawn *over* the head in its own
 *   colour, where they vanish into it, and show only where they cross the eye holes
 *   ({@link CAT_PUPILS}, {@link CAT_LIDS}).
 * - The tail already hangs free.
 */

/**
 * The scene both surfaces draw the cat in: the drawing's own coordinates, framed round the
 * cat lying on a ledge at {@link CAT_LEDGE_Y} with its tail hanging below it.
 *
 * The box is padded rather than tight because a breath scales the figure about the ledge: at
 * 1.035 the top of the ears reaches y=348.7 and the tip of the tail y=948.7, so a viewBox
 * fitted to the neutral pose would clip both ends once a breath.
 */
export const CAT_SCENE_VIEWBOX = "292 344 724 610";

/**
 * The ledge the cat lies on. It is where {@link CAT_TAIL} hangs from in the original scene,
 * because that is the underside of the desk the drawing puts the cat on.
 */
export const CAT_LEDGE_Y = 730.9;

/**
 * Lowers everything that sits on the desk onto the ledge.
 *
 * The body, the laptop and the face rest on the desk's top surface (y=668.78), and the tail
 * hangs from its underside, so the slab's own thickness, 62.12, is what brings the two to meet
 * at one line. Move one without the other and the cat either floats or the tail grows out of
 * its chest.
 */
export const CAT_ON_LEDGE = "translate(0 62.12)";

/**
 * Everything above the desk: the head, the haunch behind it, and both forelegs.
 *
 * One contour, because in the drawing they are one mass of black — the cat is lying with its
 * chin flat on the desk and its shoulders piled up behind. Nothing here is separable without
 * a cut.
 */
export const CAT_BODY =
  "M463.2 299.9C451.3 310 447.9 323.9 444.8 339.2C438.6 369.6 438.7 400 440.9 430.9C427.3 446.8 416.1 464.9 408.3 484.5C404.6 493.8 402 504 398.8 513.1C399.5 497.4 403.4 482.8 407.6 468C392.4 473.9 378.2 483.8 365.9 494.5C321.1 533.6 291.7 598.7 309.5 657.8C370.1 658.6 430.8 657.5 491.5 658.1C500.7 671.3 515.1 674.8 527.6 663.5C530.6 666 533.2 668.7 537.1 669.7C553.3 674.1 563.6 658.1 562.3 643.8C559.5 614.1 519.6 607.1 496.6 606.9C491.1 606.9 485.6 607.2 480.2 607.6C472.9 608.2 465.3 609.6 458.3 610.5C482.7 595.3 520.2 593.7 545.7 607.8C558.3 614.7 570 627 571.8 641.9C572.4 647.3 571.4 652.8 570.4 657.8C592.4 659 614.7 658.2 636.7 657.9C637.6 655.1 638.2 650.9 640.3 648.3C649.1 637.5 667.2 641.2 679.1 641.2C708.7 641.1 739 643.1 768.5 640.9C770.8 636.4 772.1 630.9 773.4 626C771.7 623 770.8 620.2 768.4 617.5C755.6 602.7 732.1 605.8 715.4 607.5C730 598.1 747.2 597.9 762.9 604.4C767 599.5 770.2 593.8 773.9 588.8C776 589.1 785.1 591.7 786.6 590.8C787.7 590.1 787.9 587.6 788.3 586.5C788.5 585.7 789.8 583.2 789.6 582.5C788.9 580.1 780.6 579.3 778.4 578.8C779.1 576.2 779.6 569.2 781.8 567.6C785.2 565 792.6 568.1 795.6 565.4C797.1 564.1 798 559.3 798.7 557.4C795.9 557.1 792 556.5 788.6 556.6C787.6 556.6 785.2 557.3 784.4 556.5C783.2 555.4 785.3 547.2 785.5 545.4C786.5 536 786.1 526.1 784.9 516.7C781.2 488.3 770 465.1 756.3 440.6C768.6 414.2 780.1 386.8 784.1 357.7C785.7 345.4 787.7 331.7 778.5 322.5C763.8 323.1 750.1 328.7 736.9 335.6C713.3 347.8 692.9 364.3 672.5 381.2C659.1 377.7 646.1 373.1 632.2 371.3C609 368.2 585.8 370.4 563 374.7C537 345.3 505.4 307.8 465.1 299.5C464 299.6 464.6 299.5 463.2 299.9Z";

/** The laptop, screen tilted towards the cat. Overlaps the far paw on purpose. */
export const CAT_LAPTOP =
  "M774.9 650.2C743.4 651.5 711.3 650.3 679.6 650.3C677.3 650.3 675 650.3 672.7 650.3C666.8 650.3 651.3 647.3 647.5 654.2C645.9 657.1 646.9 658.9 646.2 661.8C647.5 663.4 647.8 665.1 649.6 666.4C654.4 669.9 663.7 667.9 669.1 667.9C683.4 667.9 697.7 667.9 712.1 667.9C762.5 667.9 812.9 667.4 863.3 667.2C880.3 667.2 897.3 667.1 914.3 667.1C922.4 667 931.2 667.9 939.3 666.8C940 665.9 942.3 665.8 943.4 665.3C945.9 664.1 947.8 662.5 949.6 660.4C952.3 657.5 953.1 652.4 954.3 648.7C957.1 640.8 960.2 633.1 962.9 625.2C972.7 597.5 983.4 570.2 993 542.5C996.9 531.4 1009.8 509.2 998.6 499.7C990.8 493.2 977.5 496 968.3 496C947 496 925.7 496 904.4 496C889 496 873.5 496.1 858 496.1C849.5 496.1 840.7 495.1 833.1 499.8C828 503.1 826.6 508.4 824.8 513.8C821.7 523 818.1 532.2 814.8 541.4C801.7 577.5 790.2 615.1 774.9 650.2Z";

/**
 * The tail, hanging in front of the desk and curling back on itself.
 *
 * In the trace this was fused to the desk's underside — one connected region — and it is cut
 * free here because the composer draws the cat on its own top border, where the border is the
 * desk and only the tail may hang below it.
 */
export const CAT_TAIL =
  "M396.8 731.2C397.9 755.4 404.2 777.3 417.4 797.5C426.7 811.8 445.4 826.8 446.1 845.2C446.7 861.6 431.7 857.1 421.1 862.2C402.4 871.3 395.7 893.5 403.3 912.4C415.5 942.4 454.7 947.2 481.4 935.5C503.8 925.7 518.9 904.7 523.7 881.1C530 850.2 517.1 819.1 498.6 794.9C484.9 777.1 461.5 755.6 466.4 730.9Z";

/** Both half-lidded eyes, pupil rising off the lower lid. The whole expression lives here. */
export const CAT_EYES =
  "M507.6 522.6C506.9 529.9 507.1 536.2 509.6 543.4C520.5 575.2 567.7 584.5 592.7 564.4C602.6 556.4 607.3 547.3 609.4 535.2C602.7 534.1 595.7 533.8 589 533.8C588.7 539.8 589 548.1 585.2 553.4C578.9 562 566.7 562.9 561.2 552.7C557.5 545.8 558.6 537.6 558.6 530.3C541.8 527.6 524.9 525.3 508.1 522.5C507.9 522.6 507.7 522.6 507.6 522.6ZM755.1 525.4C749.2 526.7 743.2 527.8 737.4 529C737.1 537.2 738 547.2 733 554.4C727.2 562.7 716.4 561.5 710.9 553.7C707.1 548.3 708.2 539.1 708 533C697.2 533.6 686 534.4 675.3 535.4C676.1 544.7 678 553.7 684.6 560.9C701.9 579.6 732.7 577.7 748.5 558.1C756.1 548.7 757.7 536.9 757.1 525.4C756.2 525 756 525.2 755.1 525.4Z";

/** Two whiskers on the near cheek. */
export const CAT_WHISKERS =
  "M423 559.9C423.8 560.2 424.4 562 426.1 562.3C429.6 563 434.2 560.9 437.6 560.4C443.3 559.4 449.1 558.9 454.9 558.9C462 558.9 470.9 562.2 477.8 560.9C478.9 560.7 479.2 558.9 480 558.6C479.3 558 479.3 555.7 477.7 554.7C475.4 553.2 471.4 553.1 468.7 552.6C458 550.6 446.8 551 436.1 552.8C432.8 553.4 427.6 553.8 424.8 555.8C423 557.1 423.9 559 423 559.9ZM429.4 592.8C430.4 593 431 594.7 432.5 594.9C435.6 595.2 440.2 591.5 443 590.2C449.1 587.4 455.7 584.9 462.2 583.3C466.8 582.1 475.2 582.4 479 579.8C480.1 579 480.3 577.1 481 576.6C480.4 576.4 479.9 574.3 478.7 573.9C474.5 572.6 468.9 574.1 464.7 574.9C455.8 576.7 447.2 579.7 439.1 583.8C436.3 585.2 432.2 586.6 430.2 589.2C429.4 590.3 429.9 592.1 429.4 592.8Z";

/** Nose bridge and the small downturned mouth under it. */
export const CAT_MUZZLE =
  "M634.8 577C635.6 581.6 638.3 582.5 642.2 584.8C642.4 587.3 643.7 590.9 642.3 593.6C641.7 594.8 639.4 595.6 638.2 596.3C634.8 598.7 631.6 601.2 628.4 603.8C626.8 605.1 623.4 606.8 623.2 609.1C623.1 611.1 624.5 611.5 624.7 612.9C625.5 612.5 627 613.3 628 612.9C630.5 611.8 632.9 608.6 635.1 606.8C638.8 603.7 642.8 601.1 646.8 598.7C651 601.7 655 604.9 658.9 608.5C660.4 609.9 661.6 612.3 663.6 613.1C664.6 613.5 665.9 612.6 666.6 612.8C666.8 611.1 668.3 610.9 667.8 608.7C666.7 604.5 659.4 600 655.9 597.3C654.6 596.3 651.5 595.1 651.1 593.6C650.8 592.7 650.9 591.6 650.9 590.7C650.8 589.3 650.3 586.3 651.1 585C652.3 582.9 656.2 582.8 657.7 580.6C659 578.8 657.4 577.3 657.9 576C652.6 572.6 644.4 574.3 638.1 574.3C636.5 574.5 635.5 575.8 634.8 577Z";

/** Everything punched out of the head. */
export const CAT_FACE = `${CAT_EYES}${CAT_WHISKERS}${CAT_MUZZLE}`;

/** Splits a path around one of its own segments. Throws rather than cut somewhere else. */
function cutAround(path: string, segment: string): [before: string, after: string] {
  const at = path.indexOf(segment);
  if (at < 0) throw new Error(`catArt: the drawing has no segment ${segment}`);
  return [path.slice(0, at), path.slice(at + segment.length)];
}

/** The desk line from the haunch to the near paw's toes, where the near paw's cut begins. */
const DESK_UNDER_NEAR_PAW = "C370.1 658.6 430.8 657.5 491.5 658.1";

/** The chin over the near paw: the far side of the slit that parts them, where that cut ends. */
const CHIN_OVER_NEAR_PAW = "C482.7 595.3 520.2 593.7 545.7 607.8";

/** The far paw's underside, the segment where the cut round the wrist enters the contour. */
const FAR_PAW_UNDERSIDE = "C708.7 641.1 739 643.1 768.5 640.9";

/** The cheek over the far paw's toes: the far side of the slit that parts them, where that cut ends. */
const CHEEK_OVER_FAR_PAW = "C730 598.1 747.2 597.9 762.9 604.4";

const [BODY_UP_TO_NEAR_PAW, NEAR_PAW_ONWARDS] = cutAround(CAT_BODY, DESK_UNDER_NEAR_PAW);
const [NEAR_PAW_OUTLINE, BODY_BETWEEN_PAWS] = cutAround(NEAR_PAW_ONWARDS, CHIN_OVER_NEAR_PAW);
const [BODY_UP_TO_FAR_PAW, FAR_PAW_ONWARDS] = cutAround(BODY_BETWEEN_PAWS, FAR_PAW_UNDERSIDE);
const [FAR_PAW_OUTLINE, BODY_PAST_FAR_PAW] = cutAround(FAR_PAW_ONWARDS, CHEEK_OVER_FAR_PAW);

/**
 * Where the far forearm bends: the centre of the arc {@link CAT_FAR_PAW} is cut along, and so
 * the one point it can turn about without its joint opening.
 */
export const CAT_WRIST = { x: 688, y: 624 } as const;

/**
 * {@link CAT_BODY} with both forepaws cut away: everything that stays put while the cat types
 * with one paw and works the mouse with the other.
 *
 * Each cut leaves the contour where a paw's own edge starts, and comes back to it at the tip of
 * the slit that parts that paw from the face; from there on it is the drawing again. The two
 * cuts are shaped by how their paws move.
 *
 * ## Why the far paw's cut is an arc
 *
 * The far paw types: it turns about {@link CAT_WRIST}, and a circle centred on the point
 * something turns about is the one line the turn cannot move. Cut along one, and the paw's edge
 * slides along the body's at any angle, with no gap opening and no corner standing proud; cut
 * along anything else and the joint cracks open the moment the paw lifts. The underside is the
 * one edge that should open, and does — a wedge under the paw that widens towards the toes, with
 * the body's side of the arc showing through it as the round of the wrist. The cut leaves the
 * underside where that segment crosses the arc (the numbers before the arc are the segment split
 * there), and crosses the solid mass under the muzzle in a straight line to the slit.
 *
 * ## Why the near paw's cut is a straight drop
 *
 * The near paw works the mouse: it slides along the desk, which no seam survives on its own, so
 * this one is simply a vertical line from the tip of the slit down to the desk, and it is the
 * paw's overlap ({@link CAT_NEAR_PAW}) that covers it. A slide can only open a joint by as much
 * as it moves.
 */
export const CAT_BODY_WITHOUT_PAWS =
  `${BODY_UP_TO_NEAR_PAW}C359.1 658.5 408.7 657.8 458.3 657.9L458.3 610.5${CHIN_OVER_NEAR_PAW}` +
  `${BODY_UP_TO_FAR_PAW}C687.5 641.2 696 641.3 704.4 641.5A24 24 0 0 0 700 603.2L715.4 607.5` +
  `${CHEEK_OVER_FAR_PAW}${BODY_PAST_FAR_PAW}`;

/**
 * The far paw, resting on the laptop's keys: the drawing's own underside and toes, closed by
 * the cut round the wrist.
 *
 * It overlaps the body rather than meeting it. Its arc is 3 units inside the body's, and its
 * top runs up into the solid chin above the body's straight cut. Two shapes that only meet
 * leave a hairline wherever both edges are part-covered pixels, while one colour over itself is
 * invisible — so the overlap is what hides the joint at rest, and because the overlap round the
 * wrist is a ring, turning never uncovers it either.
 */
export const CAT_FAR_PAW =
  `M699.8 641.4C722.7 641.8 745.8 642.6 768.5 640.9${FAR_PAW_OUTLINE}L700 596L688 603A21 21 0 0 1 699.8 641.4Z`;

/** How far {@link CAT_NEAR_PAW} reaches back into the body past the cut. Its slide must stay inside this. */
export const CAT_NEAR_PAW_OVERLAP = 28.3;

/**
 * The near paw, folded under the chin with its toes over the front of the desk: the drawing's
 * own paw, carried {@link CAT_NEAR_PAW_OVERLAP} units back past the body's cut into the chest.
 *
 * The chest there is solid down to the desk, with the whiskers well above, so the extra length
 * cannot be seen — and it is what the cut is covered by whichever way the paw slides, up to its
 * own length. Its bottom edge is the desk line, which a sideways slide leaves where it was.
 */
export const CAT_NEAR_PAW =
  `M430 657.9C450.5 657.9 471 657.9 491.5 658.1${NEAR_PAW_OUTLINE}L430 606Z`;

/**
 * Both eyes with the pupils taken out: the whites alone, for a surface that draws its own
 * pupils over them. Each is {@link CAT_EYES}' outline with the pupil's notch replaced by the
 * straight lid line it hangs from.
 */
export const CAT_EYE_WHITES =
  "M507.6 522.6C506.9 529.9 507.1 536.2 509.6 543.4C520.5 575.2 567.7 584.5 592.7 564.4C602.6 556.4 607.3 547.3 609.4 535.2C602.7 534.1 595.7 533.8 589 533.8L558.6 530.3C541.8 527.6 524.9 525.3 508.1 522.5C507.9 522.6 507.7 522.6 507.6 522.6ZM755.1 525.4C749.2 526.7 743.2 527.8 737.4 529L708 533C697.2 533.6 686 534.4 675.3 535.4C676.1 544.7 678 553.7 684.6 560.9C701.9 579.6 732.7 577.7 748.5 558.1C756.1 548.7 757.7 536.9 757.1 525.4C756.2 525 756 525.2 755.1 525.4Z";

/**
 * The two pupils: the notches {@link CAT_EYES} cuts into the whites, carried on up past the lid
 * line into the head.
 *
 * ## Why they are drawn over the face rather than cut out of it
 *
 * They are the head's own colour, so wherever they lie on the head they cannot be seen, and
 * they show only where they cross an eye. That is what lets them move: a pupil can slide
 * anywhere in its eye, and whatever part of it passes the lid or a corner just sinks into the
 * head, where a pupil cut out of the face would need its eye redrawn round it at every
 * position. It also sets their one limit: they must stay on the head, which `catArt.test.ts`
 * checks across more than the range the stylesheet moves them.
 *
 * At rest they fill the notches exactly, so the whites less these are {@link CAT_EYES}.
 */
export const CAT_PUPILS =
  "M589 533.8C588.7 539.8 589 548.1 585.2 553.4C578.9 562 566.7 562.9 561.2 552.7C557.5 545.8 558.6 537.6 558.6 530.3L558.6 505L589 505ZM737.4 529C737.1 537.2 738 547.2 733 554.4C727.2 562.7 716.4 561.5 710.9 553.7C707.1 548.3 708.2 539.1 708 533L708 505L737.4 505Z";

/** How far above its eye each of {@link CAT_LIDS} is parked. Lowered this far, a lid's edge is its eye's own lower curve. */
export const CAT_LID_LIFT = 54;

/**
 * Both upper lids, parked {@link CAT_LID_LIFT} above their eyes on solid head, where — like the
 * pupils — they cannot be seen.
 *
 * Each is its eye's lower curve with a flat top. Lowered, it closes the eye from the top down,
 * and because its edge is the eye's own lower curve, what is left half-way is a crescent the
 * shape of the eye: a drowsy cat rather than a shutter coming down.
 *
 * Two liberties are taken for antialiasing rather than looks. A lid lowered exactly onto its
 * eye leaves a ghost of the eye's outline, since the hole's edge and the lid's are then both
 * part-covered pixels on the same line; so a closed lid goes on 8 units past the eye's bottom,
 * and each curve is stretched 15% sideways about its eye's centre so that it also clears the
 * corners, where the eye's edge is nearly vertical and lowering moves nothing sideways. And the
 * near lid's outer top corner is cut back, because parked it would otherwise stand out past the
 * notch where the near ear meets the cheek.
 */
export const CAT_LIDS =
  "M500 468.6C499.2 475.9 499.4 482.2 502.3 489.4C514.8 521.2 569.1 530.5 597.8 510.4C609.2 502.4 614.6 493.3 617 481.2L617 451L500 451ZM669.2 481.4C670.1 490.7 672.3 499.7 679.9 506.9C699.8 525.6 735.2 523.7 753.3 504.1C762.1 494.7 763.9 482.9 763.2 471.4L750 451L669.2 451Z";
