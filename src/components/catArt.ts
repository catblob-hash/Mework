/**
 * The one cat the product is drawn with, as path data.
 *
 * Mework's artwork is a single drawing in five places: the boxed application icon, the brand
 * mark in the sidebar, the favicon, the cat loafing on a draft composer's rim, and the cat
 * that walks beside a streaming round. Redrawing it five times is how five different cats
 * happen, so the drawing lives here once and every surface scales a piece of it into place.
 *
 * All coordinates are in the artwork's own drawing space, the one `src/logo.svg` is authored
 * in. The strings below therefore appear verbatim inside `src/logo.svg`,
 * `src/mework-icon.svg`, `src/mework-icon-small.svg` and `src/mework-mark.svg`, which is what
 * `MeworkIcon.test.tsx` pins: the shipped files and the in-app copies cannot drift.
 *
 * ## Why the scene is one even-odd path
 *
 * {@link CAT_SCENE} is drawn as a single path with `fill-rule="evenodd"`, never as stacked
 * shapes. The overlaps carry the drawing: where the near paw crosses the laptop the two
 * subpaths cancel and leave the white gap that puts the paw in front, and the same trick
 * seats the chin on the desk. Splitting the scene into separately filled shapes fills those
 * gaps back in.
 *
 * Even-odd also only *clears* a subpath lying inside the outer contour. One that crossed the
 * cheek line would be filled instead, and a whisker would render as a spike growing out of
 * the silhouette — which is the constraint {@link CAT_HEAD} is cut to satisfy.
 *
 * ## Why the head is a separate outline
 *
 * In the scene the head is fused to the haunch behind it and to the forelegs in front of it;
 * they are one connected region, parted only by the thin slash the artwork draws at the top
 * of the cheek. Surfaces that show the head alone — every brand slot below about 32px, and
 * the streaming cat, which nods it — need it closed, so {@link CAT_HEAD} carries a jaw that
 * the scene never shows because the paw covers it. It bulges far enough left to hold the near
 * whiskers; a tighter jaw leaves them outside the contour and they fill in as spikes.
 */

/** Extent of the whole scene: the desk, the cat on it, its tail, and the laptop. */
export const CAT_SCENE_BOX = { x: 274.9, y: 299.4, width: 728.3, height: 695 } as const;

/** Extent of {@link CAT_HEAD} on its own, ears included. */
export const CAT_HEAD_BOX = { x: 388, y: 299.5, width: 410.7, height: 362.5 } as const;

/** Centre of the head's bounding box, and the origin every surface scales about. */
export const CAT_HEAD_CENTER = { x: 593.4, y: 480.8 } as const;

/**
 * Everything above the desk: the head, the haunch behind it, and both forelegs.
 *
 * One contour, because in the drawing they are one mass of black — the cat is lying with its
 * chin flat on the desk and its shoulders piled up behind. Nothing here is separable without
 * a cut; {@link CAT_HEAD} is the one cut the product makes.
 */
export const CAT_BODY =
  "M463.2 299.9C451.3 310 447.9 323.9 444.8 339.2C438.6 369.6 438.7 400 440.9 430.9C427.3 446.8 416.1 464.9 408.3 484.5C404.6 493.8 402 504 398.8 513.1C399.5 497.4 403.4 482.8 407.6 468C392.4 473.9 378.2 483.8 365.9 494.5C321.1 533.6 291.7 598.7 309.5 657.8C370.1 658.6 430.8 657.5 491.5 658.1C500.7 671.3 515.1 674.8 527.6 663.5C530.6 666 533.2 668.7 537.1 669.7C553.3 674.1 563.6 658.1 562.3 643.8C559.5 614.1 519.6 607.1 496.6 606.9C491.1 606.9 485.6 607.2 480.2 607.6C472.9 608.2 465.3 609.6 458.3 610.5C482.7 595.3 520.2 593.7 545.7 607.8C558.3 614.7 570 627 571.8 641.9C572.4 647.3 571.4 652.8 570.4 657.8C592.4 659 614.7 658.2 636.7 657.9C637.6 655.1 638.2 650.9 640.3 648.3C649.1 637.5 667.2 641.2 679.1 641.2C708.7 641.1 739 643.1 768.5 640.9C770.8 636.4 772.1 630.9 773.4 626C771.7 623 770.8 620.2 768.4 617.5C755.6 602.7 732.1 605.8 715.4 607.5C730 598.1 747.2 597.9 762.9 604.4C767 599.5 770.2 593.8 773.9 588.8C776 589.1 785.1 591.7 786.6 590.8C787.7 590.1 787.9 587.6 788.3 586.5C788.5 585.7 789.8 583.2 789.6 582.5C788.9 580.1 780.6 579.3 778.4 578.8C779.1 576.2 779.6 569.2 781.8 567.6C785.2 565 792.6 568.1 795.6 565.4C797.1 564.1 798 559.3 798.7 557.4C795.9 557.1 792 556.5 788.6 556.6C787.6 556.6 785.2 557.3 784.4 556.5C783.2 555.4 785.3 547.2 785.5 545.4C786.5 536 786.1 526.1 784.9 516.7C781.2 488.3 770 465.1 756.3 440.6C768.6 414.2 780.1 386.8 784.1 357.7C785.7 345.4 787.7 331.7 778.5 322.5C763.8 323.1 750.1 328.7 736.9 335.6C713.3 347.8 692.9 364.3 672.5 381.2C659.1 377.7 646.1 373.1 632.2 371.3C609 368.2 585.8 370.4 563 374.7C537 345.3 505.4 307.8 465.1 299.5C464 299.6 464.6 299.5 463.2 299.9Z";

/** The laptop, screen tilted towards the cat. Overlaps the far paw on purpose. */
export const CAT_LAPTOP =
  "M774.9 650.2C743.4 651.5 711.3 650.3 679.6 650.3C677.3 650.3 675 650.3 672.7 650.3C666.8 650.3 651.3 647.3 647.5 654.2C645.9 657.1 646.9 658.9 646.2 661.8C647.5 663.4 647.8 665.1 649.6 666.4C654.4 669.9 663.7 667.9 669.1 667.9C683.4 667.9 697.7 667.9 712.1 667.9C762.5 667.9 812.9 667.4 863.3 667.2C880.3 667.2 897.3 667.1 914.3 667.1C922.4 667 931.2 667.9 939.3 666.8C940 665.9 942.3 665.8 943.4 665.3C945.9 664.1 947.8 662.5 949.6 660.4C952.3 657.5 953.1 652.4 954.3 648.7C957.1 640.8 960.2 633.1 962.9 625.2C972.7 597.5 983.4 570.2 993 542.5C996.9 531.4 1009.8 509.2 998.6 499.7C990.8 493.2 977.5 496 968.3 496C947 496 925.7 496 904.4 496C889 496 873.5 496.1 858 496.1C849.5 496.1 840.7 495.1 833.1 499.8C828 503.1 826.6 508.4 824.8 513.8C821.7 523 818.1 532.2 814.8 541.4C801.7 577.5 790.2 615.1 774.9 650.2Z";

/** The desk: one slab and two legs. The tail is cut out of it, so this is the desk alone. */
export const CAT_DESK =
  "M310.6 731.5C310.6 791.8 310.5 852.1 310.5 912.4C310.5 929.2 310.5 946 310.5 962.7C310.5 968.7 308.4 978.8 310.5 984.6C315.7 999.4 334.6 994.3 347.5 994.3C352.1 994.3 360.3 996 364.4 992.7C371.2 987.5 369.4 979.8 369.4 972.2C369.4 957.2 369.4 942.2 369.4 927.2C369.5 861.9 368.9 796.7 369.5 731.4C376.9 728.7 389 730.4 396.8 731.2C560.7 731 655 731 749.3 731C779.7 731 810.1 731 840.5 731C855 731 870.7 729.4 885.1 731.1C886.4 736.8 885.5 744.1 885.5 750.1C885.5 762.4 885.5 774.7 885.5 786.9C885.4 825.5 885.4 864 885.3 902.6C885.3 921.1 885.3 939.7 885.3 958.3C885.3 964.3 885.3 970.4 885.3 976.5C885.3 978.5 885.3 980.6 885.3 982.7C885.3 983.8 884.5 986.4 885.4 985.9C890.1 999.1 909.6 994.4 920.3 994.4C927.3 994.4 936.6 996.2 941.6 990.1C945 986 944 980.5 944 975.6C944.1 965.8 944.1 956 944.1 946.2C944.1 898 944.1 849.8 944 801.6C944 785.6 944 769.6 944 753.6C944 746.7 942.7 738.1 944.2 731.3C953 730.9 963.9 732.5 971.7 727.3C980.9 721.1 981.4 712.1 981.4 702C981.4 690.8 981.6 682.3 972.5 674.5C966.8 669.7 959 670.3 952.1 670.3C939.8 670.3 927.5 670.2 915.2 670.2C819 670.1 722.9 670.1 626.8 669.8C611.5 669.8 596.2 669.7 580.9 669.7C576.2 669.7 566.5 667.7 562.1 669.6C558.9 671 556.3 673.5 553 675C544.1 679.1 535.6 676.6 527.4 672.5C519 677 509.9 679.1 500.8 674.8C498.3 673.6 496 670.8 493.6 669.9C487.2 667.7 476.2 669.6 469.7 669.6C430.1 669.6 390.5 669.6 350.9 669.6C338.4 669.6 325.9 669.6 313.4 669.6C306.2 669.6 298.5 668.8 291.4 670.5C275.6 674.3 274.8 689.9 274.9 703.3C274.9 710.5 274.8 717.8 280.3 723.4C289.1 732.2 299.6 730.2 310.6 731.5Z";

/**
 * The tail, hanging in front of the desk and curling back on itself.
 *
 * In the trace this was fused to the desk's underside — one connected region — and it is cut
 * free here because the composer draws the cat on its own top border, where the border is the
 * desk and only the tail may hang below it.
 */
export const CAT_TAIL =
  "M396.8 731.2C397.9 755.4 404.2 777.3 417.4 797.5C426.7 811.8 445.4 826.8 446.1 845.2C446.7 861.6 431.7 857.1 421.1 862.2C402.4 871.3 395.7 893.5 403.3 912.4C415.5 942.4 454.7 947.2 481.4 935.5C503.8 925.7 518.9 904.7 523.7 881.1C530 850.2 517.1 819.1 498.6 794.9C484.9 777.1 461.5 755.6 466.4 730.9Z";

/**
 * The head alone, closed across the jaw the scene hides behind the near paw.
 *
 * Used on its own for the brand mark's offset drop shadow, and as the outer contour of
 * {@link CAT_GLYPH}. The jaw is what holds the near whiskers inside the contour — they clear
 * it by 30.7 units, against 27.4 for the muzzle, the tightest thing on the face — so a jaw
 * drawn closer to the cheek turns them into spikes. `MeworkIcon.test.tsx` scans for it.
 */
export const CAT_HEAD =
  "M570.4 657.8C592.4 659 614.7 658.2 636.7 657.9C637.6 655.1 638.2 650.9 640.3 648.3C649.1 637.5 667.2 641.2 679.1 641.2C708.7 641.1 739 643.1 768.5 640.9C770.8 636.4 772.1 630.9 773.4 626C771.7 623 770.8 620.2 768.4 617.5C755.6 602.7 732.1 605.8 715.4 607.5C730 598.1 747.2 597.9 762.9 604.4C767 599.5 770.2 593.8 773.9 588.8C776 589.1 785.1 591.7 786.6 590.8C787.7 590.1 787.9 587.6 788.3 586.5C788.5 585.7 789.8 583.2 789.6 582.5C788.9 580.1 780.6 579.3 778.4 578.8C779.1 576.2 779.6 569.2 781.8 567.6C785.2 565 792.6 568.1 795.6 565.4C797.1 564.1 798 559.3 798.7 557.4C795.9 557.1 792 556.5 788.6 556.6C787.6 556.6 785.2 557.3 784.4 556.5C783.2 555.4 785.3 547.2 785.5 545.4C786.5 536 786.1 526.1 784.9 516.7C781.2 488.3 770 465.1 756.3 440.6C768.6 414.2 780.1 386.8 784.1 357.7C785.7 345.4 787.7 331.7 778.5 322.5C763.8 323.1 750.1 328.7 736.9 335.6C713.3 347.8 692.9 364.3 672.5 381.2C659.1 377.7 646.1 373.1 632.2 371.3C609 368.2 585.8 370.4 563 374.7C537 345.3 505.4 307.8 465.1 299.5C464 299.6 464.6 299.5 463.2 299.9C451.3 310 447.9 323.9 444.8 339.2C438.6 369.6 438.7 400 440.9 430.9C427.3 446.8 416.1 464.9 408.3 484.5C404.6 493.8 402 504 398.8 513.1C380.9 564 386.8 589.1 404.8 611.1C432.8 649.1 474.4 671.8 570.4 657.8Z";

/**
 * The skull alone: the same head with a flat jaw and no ears.
 *
 * {@link CAT_HEAD} closes across a jaw the scene hides behind the near paw, and that jaw is
 * a bulge rather than a chin — it is cut to keep the whiskers inside the contour, not to be
 * looked at. On a cat that stands up and holds its head clear of everything, the bulge is the
 * first thing the eye lands on. This outline replaces it with the flat, wide jaw a cat head
 * actually has from the front, and drops the ears out of the contour so they can be animated
 * as the separate shapes they are on a real cat.
 *
 * It is the brand cat still: the face is unchanged, and the outline is drawn to the same box
 * around the same centre, so {@link catHeadTransform} places this and {@link CAT_HEAD}
 * interchangeably. Without ears the box stops at y=372 rather than 299.5.
 */
export const CAT_HEAD_FLAT =
  "M424 470C428 424 452 396 492 384C528 373 570 372 606 378C646 385 690 398 724 420C762 445 790 476 796 520C801 558 796 606 768 636C734 664 672 674 606 672C540 670 486 656 456 620C428 586 418 518 424 470Z";

/**
 * The far ear, as its own shape.
 *
 * Both ears are drawn before the face, never after: the face is an even-odd path whose eyes
 * are holes, and a shape painted over one would plug it — a shape painted under one shows
 * through it, which is why neither ear may reach below y=500 however far it swings.
 *
 * ## Why the base is buried rather than butted
 *
 * An ear that swings is a rigid shape turning on a pivot while the skull under it stays put,
 * so the only thing keeping the two fused is that the ear's base is *inside* the skull with
 * room to move. Both base corners sit far enough in that the whole swing keeps them there:
 * the ear's outline leaves the skull exactly once, over the tip, and a second excursion is
 * the shape a crack takes. Draw the base out at the skull's edge instead — as the first
 * version of these did on the near side, where the outer corner stood ten units clear of the
 * contour — and the corner becomes a spur with a white wedge under it that opens into a gash
 * the moment the ear turns.
 */
export const CAT_EAR_FAR =
  "M452 482C434 424 428 356 436 302C480 334 532 370 578 408C542 428 488 450 452 482Z";

/** The near ear: the same shape mirrored, a little larger for the three-quarter turn. */
export const CAT_EAR_NEAR =
  "M754 468C776 418 804 348 800 292C756 328 706 366 666 422C700 438 730 456 754 468Z";

/** Both half-lidded eyes, pupil rising off the lower lid. The whole expression lives here. */
export const CAT_EYES =
  "M507.6 522.6C506.9 529.9 507.1 536.2 509.6 543.4C520.5 575.2 567.7 584.5 592.7 564.4C602.6 556.4 607.3 547.3 609.4 535.2C602.7 534.1 595.7 533.8 589 533.8C588.7 539.8 589 548.1 585.2 553.4C578.9 562 566.7 562.9 561.2 552.7C557.5 545.8 558.6 537.6 558.6 530.3C541.8 527.6 524.9 525.3 508.1 522.5C507.9 522.6 507.7 522.6 507.6 522.6ZM755.1 525.4C749.2 526.7 743.2 527.8 737.4 529C737.1 537.2 738 547.2 733 554.4C727.2 562.7 716.4 561.5 710.9 553.7C707.1 548.3 708.2 539.1 708 533C697.2 533.6 686 534.4 675.3 535.4C676.1 544.7 678 553.7 684.6 560.9C701.9 579.6 732.7 577.7 748.5 558.1C756.1 548.7 757.7 536.9 757.1 525.4C756.2 525 756 525.2 755.1 525.4Z";

/** Two whiskers on the near cheek. Dropped below roughly a 32px head, where they are dirt. */
export const CAT_WHISKERS =
  "M423 559.9C423.8 560.2 424.4 562 426.1 562.3C429.6 563 434.2 560.9 437.6 560.4C443.3 559.4 449.1 558.9 454.9 558.9C462 558.9 470.9 562.2 477.8 560.9C478.9 560.7 479.2 558.9 480 558.6C479.3 558 479.3 555.7 477.7 554.7C475.4 553.2 471.4 553.1 468.7 552.6C458 550.6 446.8 551 436.1 552.8C432.8 553.4 427.6 553.8 424.8 555.8C423 557.1 423.9 559 423 559.9ZM429.4 592.8C430.4 593 431 594.7 432.5 594.9C435.6 595.2 440.2 591.5 443 590.2C449.1 587.4 455.7 584.9 462.2 583.3C466.8 582.1 475.2 582.4 479 579.8C480.1 579 480.3 577.1 481 576.6C480.4 576.4 479.9 574.3 478.7 573.9C474.5 572.6 468.9 574.1 464.7 574.9C455.8 576.7 447.2 579.7 439.1 583.8C436.3 585.2 432.2 586.6 430.2 589.2C429.4 590.3 429.9 592.1 429.4 592.8Z";

/** Nose bridge and the small downturned mouth under it. */
export const CAT_MUZZLE =
  "M634.8 577C635.6 581.6 638.3 582.5 642.2 584.8C642.4 587.3 643.7 590.9 642.3 593.6C641.7 594.8 639.4 595.6 638.2 596.3C634.8 598.7 631.6 601.2 628.4 603.8C626.8 605.1 623.4 606.8 623.2 609.1C623.1 611.1 624.5 611.5 624.7 612.9C625.5 612.5 627 613.3 628 612.9C630.5 611.8 632.9 608.6 635.1 606.8C638.8 603.7 642.8 601.1 646.8 598.7C651 601.7 655 604.9 658.9 608.5C660.4 609.9 661.6 612.3 663.6 613.1C664.6 613.5 665.9 612.6 666.6 612.8C666.8 611.1 668.3 610.9 667.8 608.7C666.7 604.5 659.4 600 655.9 597.3C654.6 596.3 651.5 595.1 651.1 593.6C650.8 592.7 650.9 591.6 650.9 590.7C650.8 589.3 650.3 586.3 651.1 585C652.3 582.9 656.2 582.8 657.7 580.6C659 578.8 657.4 577.3 657.9 576C652.6 572.6 644.4 574.3 638.1 574.3C636.5 574.5 635.5 575.8 634.8 577Z";

/** Everything punched out of the head. */
export const CAT_FACE = `${CAT_EYES}${CAT_WHISKERS}${CAT_MUZZLE}`;

/** The head with its whole face appended as counter-contours. */
export const CAT_GLYPH = `${CAT_HEAD}${CAT_FACE}`;

/** The head without whiskers — the reading for small surfaces. */
export const CAT_GLYPH_PLAIN = `${CAT_HEAD}${CAT_EYES}${CAT_MUZZLE}`;

/**
 * The flat-jawed head with the face punched out of it: the streaming cat's whole head.
 *
 * Whiskers are left out for the same reason {@link CAT_GLYPH_PLAIN} leaves them out — at the
 * size this is drawn one is a quarter of a pixel — and the ears are left out because they
 * move, so they are drawn as {@link CAT_EAR_NEAR} and {@link CAT_EAR_FAR} underneath it.
 */
export const CAT_GLYPH_FLAT = `${CAT_HEAD_FLAT}${CAT_EYES}${CAT_MUZZLE}`;

/**
 * The whole drawing, as the single even-odd path it has to be.
 *
 * The far side of the cheek and the laptop's near corner are drawn by subpaths cancelling,
 * so this order is free but the composition is not: dropping any piece changes shapes
 * elsewhere in the drawing, not just its own.
 */
export const CAT_SCENE = `${CAT_BODY}${CAT_LAPTOP}${CAT_FACE}${CAT_DESK}${CAT_TAIL}`;

/**
 * Places the head at `(x, y)` in a surface's own viewBox, at `scale` units per drawing unit.
 *
 * `transform-box` does not enter into it: this is a presentation transform on the element, so
 * its coordinates are the parent viewBox's. A CSS animation that also drives `transform` on
 * the same element would replace this, so surfaces that animate the head put it on a wrapping
 * `<g>`.
 */
export function catHeadTransform(x: number, y: number, scale: number): string {
  return `translate(${x} ${y}) scale(${scale}) translate(${-CAT_HEAD_CENTER.x} ${-CAT_HEAD_CENTER.y})`;
}
